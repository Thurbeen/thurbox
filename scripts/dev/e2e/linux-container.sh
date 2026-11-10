#!/usr/bin/env bash
#
# A throwaway Podman container running sshd + tmux + git, to point a remote
# host at by hand — the ephemeral-Linux member of the e2e family (see
# scripts/dev/README.md). Nothing touches your real ~/.ssh or ~/.config; all
# state lives under target/remote-ssh-test/ (gitignored).
#
# Usage:
#   scripts/dev/e2e/linux-container.sh up        # build image + start container
#   scripts/dev/e2e/linux-container.sh hosts     # print the hosts.toml block for manual TUI testing
#   scripts/dev/e2e/linux-container.sh ssh       # open a shell on the container
#   scripts/dev/e2e/linux-container.sh down      # remove the container
#   scripts/dev/e2e/linux-container.sh clean     # remove container + all local state
#
# Env overrides: THURBOX_SSH_TEST_PORT (default 2222),
#                THURBOX_SSH_TEST_DIR  (default <repo>/target/remote-ssh-test)
#
# Requires: podman, ssh-keygen.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
# shellcheck source=scripts/dev/e2e/lib/e2e-common.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/e2e/lib/e2e-common.sh"

WORKDIR="${THURBOX_SSH_TEST_DIR:-$REPO_ROOT/target/remote-ssh-test}"
IMAGE="thurbox-remote-test"
CONTAINER="thurbox-remote"
PORT="${THURBOX_SSH_TEST_PORT:-2222}"
KEY="$WORKDIR/id_ed25519"

ssh_remote() {
  ssh -p "$PORT" -i "$KEY" \
    -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=4 \
    root@localhost "$@"
}

# The ssh_opts array pointing at the container. Absolute paths only — thurbox
# passes ssh_opts to `ssh` via Command (no shell ~ expansion).
container_ssh_opts() {
  cat <<EOF
[
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

# Emit the container's hosts.toml [[hosts]] block via the shared emitter.
container_hosts_block() { hosts_block podman root@localhost "$(container_ssh_opts)"; }

cmd_up() {
  command -v podman >/dev/null || die "podman not found"
  mkdir -p "$WORKDIR"
  if [ ! -f "$KEY" ]; then
    log "generating throwaway keypair at $KEY"
    ssh-keygen -t ed25519 -N "" -C thurbox-remote-test -f "$KEY" >/dev/null
  fi
  cp "$KEY.pub" "$WORKDIR/authorized_keys"

  cat > "$WORKDIR/Containerfile" <<'EOF'
FROM docker.io/library/debian:trixie-slim
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
  container_hosts_block
}

cmd_hosts() { container_hosts_block; }

cmd_ssh() { ssh_remote "${@:-bash -l}"; }

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
  hosts) cmd_hosts ;;
  ssh)   shift; cmd_ssh "$@" ;;
  down)  cmd_down ;;
  clean) cmd_clean ;;
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
