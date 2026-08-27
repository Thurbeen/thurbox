#!/usr/bin/env bash
#
# e2e: thurbox's remote-SSH backend against a throwaway Podman container running
# sshd + tmux + git — the ephemeral-Linux member of the e2e family (see
# scripts/dev/README.md). Nothing touches your real ~/.ssh or ~/.config; all
# state lives under target/remote-ssh-test/ (gitignored) plus an isolated XDG
# home in a temp dir for the automated run.
#
# Usage:
#   scripts/dev/e2e/linux-container.sh up        # build image + start container
#   scripts/dev/e2e/linux-container.sh test      # isolated headless e2e (asserts ssh:podman)
#   scripts/dev/e2e/linux-container.sh hosts     # print the hosts.toml block for manual TUI testing
#   scripts/dev/e2e/linux-container.sh ssh       # open a shell on the container
#   scripts/dev/e2e/linux-container.sh down      # remove the container
#   scripts/dev/e2e/linux-container.sh clean     # remove container + all local state
#
# Env overrides: THURBOX_SSH_TEST_PORT (default 2222),
#                THURBOX_SSH_TEST_DIR  (default <repo>/target/remote-ssh-test)
#
# Requires: podman, ssh-keygen, cargo. (No python3 — JSON is parsed in-shell.)

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
REMOTE_REPO="/srv/repo"

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

# Remove any worktrees/branches/tmux state the test left on the container.
remote_reset() {
  # shellcheck disable=SC2016 # body runs on the remote; vars must stay unexpanded locally
  ssh_remote '
    cd /srv/repo || exit 0
    git worktree list --porcelain | awk "/^worktree/ {print \$2}" | grep -v "^/srv/repo$" \
      | while read -r w; do git worktree remove --force "$w"; done
    git worktree prune
    git for-each-ref --format="%(refname:short)" "refs/heads/test/*" \
      | while read -r b; do git branch -D "$b"; done
    rm -rf /root/.codex /root/.local/share/thurbox /root/.local/share/thurbox-dev /root/.config/thurbox-dev
    tmux -L thurbox-dev kill-server 2>/dev/null || true
  ' 2>/dev/null || true
}

# The container's own thurbox-cli: the dev binary the first shared spawn
# provisions there (the container is the same platform as this machine), under
# the dev flavour's directory — a release laptop would use thurbox/bin.
REMOTE_CLI='/root/.local/share/thurbox-dev/bin/thurbox-cli'
remote_cli() { ssh_remote "$REMOTE_CLI --json $*"; }

# The sharing half of the e2e: the container starts with NO thurbox. The first
# `session create --host podman` must provision the CLI and create the session
# through it, after which both directions — a session created inside the
# container, one created from here — are one list, and delete/restore/relaunch
# travel both ways. Runs inside cmd_test's isolated XDG home.
shared_sessions_probe() {
  log "asserting shared sessions (provisioning + both directions)"
  # Sharing is the default, and `hosts_block` writes no `share_sessions`, so the
  # sessions created above were already delegated: the CLI must be there.
  if ssh_remote "test -x $REMOTE_CLI"; then
    ok "thurbox-cli provisioned under ~/.local/share/thurbox-dev/bin on the container"
  else
    bad "no thurbox-cli provisioned on the container"
    return
  fi
  case "$(remote_cli version)" in
    *'"schema_version"'*) ok "provisioned CLI answers version --json with a schema" ;;
    *) bad "provisioned CLI does not answer version --json" ;;
  esac
  # The host's database is the record: the sessions created from here are in it.
  case "$(remote_cli session list)" in
    *'"name":"e2e"'*) ok "a session created from here is in the container's own database" ;;
    *) bad "the container's database does not list the session created from here" ;;
  esac

  # A session created INSIDE the container, by its own CLI, with its own
  # agents.toml (seeded by the first delegated create; `shell` is added here).
  ssh_remote "mkdir -p /root/.config/thurbox-dev && printf 'default = \"shell\"\n[[agents]]\nname = \"shell\"\ncommand = \"bash\"\n' >> /root/.config/thurbox-dev/agents.toml"
  local inside_id
  inside_id="$(remote_cli session create --name e2e-inside --repo-path "$REMOTE_REPO" --agent shell | json_field id)"
  if [ -n "$inside_id" ]; then
    ok "a session was created inside the container by its own CLI ($inside_id)"
  else
    bad "session create inside the container failed"
    return
  fi
  local sync_out
  sync_out="$(e2e_cli session sync --host podman)" || die "session sync failed"
  case "$sync_out" in
    *"$inside_id"*) ok "session sync adopted the session created inside" ;;
    *) bad "session sync did not report the inside session (got: $sync_out)" ;;
  esac
  case "$(e2e_cli session get "$inside_id")" in
    *'"backend_type":"ssh:podman"'*) ok "the adopted session is on ssh:podman with the host's id" ;;
    *) bad "the adopted session is not listed here on ssh:podman" ;;
  esac

  # Delete from here, forced: the host tears down and its deleted list shows it.
  e2e_cli session delete "$inside_id" --force >/dev/null || die "delegated delete failed"
  case "$(remote_cli session list --deleted)" in
    *"$inside_id"*) ok "a force-delete from here lands in the container's deleted list" ;;
    *) bad "the container does not list the deleted session" ;;
  esac

  # Soft-delete inside, restore from here: the host relaunches, both sides agree.
  local e2e_id
  e2e_id="$(e2e_cli session list | json_field id)"
  remote_cli session delete "$e2e_id" >/dev/null || die "soft delete inside the container failed"
  e2e_cli session sync --host podman >/dev/null
  case "$(e2e_cli session list --deleted)" in
    *"$e2e_id"*) ok "a soft-delete inside the container is mirrored here" ;;
    *) bad "the soft-delete inside the container was not mirrored" ;;
  esac
  e2e_cli session restore "$e2e_id" --best-effort >/dev/null || die "delegated restore failed"
  case "$(remote_cli session list)" in
    *"$e2e_id"*) ok "a restore from here brings the session back on the container" ;;
    *) bad "the container does not list the restored session" ;;
  esac

  # A host restart: the tmux server is gone, the database is not. A relaunch
  # asked for from here is the host's, and asking twice launches once.
  podman restart "$CONTAINER" >/dev/null
  for _ in $(seq 1 20); do ssh_remote true 2>/dev/null && break; sleep 1; done
  e2e_cli session restart "$e2e_id" --if-missing >/dev/null || die "relaunch after restart failed"
  e2e_cli session restart "$e2e_id" --if-missing >/dev/null || die "second relaunch failed"
  local windows
  windows="$(ssh_remote 'tmux -L thurbox-dev list-windows -t thurbox-dev -F "#{window_name}" 2>/dev/null' | grep -c '^tb-e2e$' || true)"
  if [ "$windows" = "1" ]; then
    ok "after a container restart the session was relaunched exactly once"
  else
    bad "expected one tb-e2e window after the relaunch, found ${windows:-0}"
  fi
}

cmd_test() {
  command -v cargo >/dev/null || die "cargo not found"
  ssh_remote true 2>/dev/null || die "container not reachable — run '$0 up' first"

  # Fully isolated XDG home: never touches the user's real config/db, and no
  # running TUI watches this database.
  local xdg; xdg="$(mktemp -d)"
  trap 'rm -rf "$xdg"; remote_reset' RETURN
  mkdir -p "$xdg/config/thurbox-dev/hooks"
  container_hosts_block > "$xdg/config/thurbox-dev/hosts.toml"
  # `codex` (a config-dir-hooked agent name) exercises the remote hook
  # provisioning; `clauded` carries a thurbox-managed config file as a launch
  # arg (claude's `--settings` shape) exercising the arg materialization. Both
  # just run bash — only the *name*/args drive the remote wiring under test.
  cat > "$xdg/config/thurbox-dev/agents.toml" <<EOF
default = "shell"
[[agents]]
name = "shell"
command = "bash"
[[agents]]
name = "codex"
command = "bash"
[[agents]]
name = "clauded"
command = "bash"
args = ["$xdg/config/thurbox-dev/hooks/claude.json"]
EOF
  # A claude-shaped hooks file carrying the signal marker the remote rewrite
  # keys on.
  cat > "$xdg/config/thurbox-dev/hooks/claude.json" <<'EOF'
{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state done || true"}]}]}}
EOF

  remote_reset
  log "creating a remote session via thurbox-cli (isolated DB)"
  export XDG_CONFIG_HOME="$xdg/config" XDG_DATA_HOME="$xdg/data"
  # Running this script from inside a thurbox session injects THURBOX_*_DIR
  # overrides that outrank XDG_* (paths.rs) and would point the CLI at the
  # real config/DB — pin them to the sandbox (same pattern as
  # scripts/dev/lib/sandbox-env.sh).
  export THURBOX_CONFIG_DIR="$xdg/config/thurbox-dev" THURBOX_DATA_DIR="$xdg/data/thurbox-dev"
  local result backend
  result="$(e2e_create_and_get \
    --name e2e --host podman --repo-path "$REMOTE_REPO" \
    --agent shell --worktree-branch test/e2e --base-branch main)" || exit 1
  read -r _ backend <<<"$result"

  # Confirm the artifacts really live on the remote.
  local remote_window remote_wt
  remote_window="$(ssh_remote 'tmux -L thurbox-dev list-windows -t thurbox-dev -F "#{window_name}" 2>/dev/null' | grep -c '^tb-e2e$' || true)"
  remote_wt="$(ssh_remote 'cd /srv/repo && git worktree list | grep -c test-e2e' 2>/dev/null || echo 0)"

  # --- remote hooks-driven status wiring ------------------------------------
  # A codex spawn must ship the rewritten hooks payload into the host's
  # ~/.codex (created here = "agent installed"), idempotently across spawns;
  # the arg-carried config file must land rewritten at its mirrored path.
  log "asserting remote hook provisioning (codex config-dir + arg-carried file)"
  FAILS=0
  ssh_remote 'mkdir -p /root/.codex'
  e2e_cli session create --name e2e-codex --host podman --repo-path "$REMOTE_REPO" \
    --agent codex --worktree-branch test/e2e-codex --base-branch main >/dev/null \
    || die "codex session create failed"
  local hooks_json
  hooks_json="$(ssh_remote 'cat /root/.codex/hooks.json 2>/dev/null' || true)"
  case "$hooks_json" in
    *"tmux set-option -p @thurbox_state"*) ok "codex hooks.json shipped rewritten" ;;
    *) bad "codex hooks.json missing or not rewritten" ;;
  esac
  case "$hooks_json" in
    *thurbox-cli*) bad "codex hooks.json still references thurbox-cli" ;;
    *) ok "no thurbox-cli reference survives the rewrite" ;;
  esac
  # Second spawn (fresh process, so no in-process cache): byte-stable file.
  e2e_cli session create --name e2e-codex2 --host podman --repo-path "$REMOTE_REPO" \
    --agent codex --worktree-branch test/e2e-codex2 --base-branch main >/dev/null \
    || die "second codex session create failed"
  if [ "$(ssh_remote 'cat /root/.codex/hooks.json 2>/dev/null' || true)" = "$hooks_json" ]; then
    ok "re-provisioning is idempotent (byte-stable hooks.json)"
  else
    bad "second spawn changed hooks.json (merge not idempotent)"
  fi
  e2e_cli session create --name e2e-arg --host podman --repo-path "$REMOTE_REPO" \
    --agent clauded --worktree-branch test/e2e-arg --base-branch main >/dev/null \
    || die "arg-carried session create failed"
  # The local config root is outside $HOME, so it mirrors at the same path.
  # shellcheck disable=SC2029 # $xdg expands locally on purpose (it names the remote mirror path)
  case "$(ssh_remote "cat '$xdg/config/thurbox-dev/hooks/claude.json' 2>/dev/null" || true)" in
    *"tmux set-option -p @thurbox_state done"*) ok "arg-carried config materialized rewritten" ;;
    *) bad "arg-carried config missing or not rewritten on the host" ;;
  esac

  # --- headless remote-status poll (automation tick) --------------------------
  # With no TUI attached (no control-mode subscription alive), an
  # `automation tick` must pull the pane's @thurbox_state option into the DB —
  # visible as `hook_state` in `session list --json`. Simulate the rewritten
  # hook firing by setting the option on every remote pane, exactly as the
  # in-pane command would.
  log "asserting the headless remote-status poll (tick pulls @thurbox_state)"
  # shellcheck disable=SC2016 # $p expands on the remote side on purpose
  ssh_remote 'tmux -L thurbox-dev list-panes -s -t thurbox-dev -F "#{pane_id}" 2>/dev/null \
    | while read -r p; do tmux -L thurbox-dev set-option -p -t "$p" @thurbox_state working; done'
  e2e_cli automation tick >/dev/null || die "automation tick failed"
  case "$(e2e_cli session list)" in
    *'"hook_state":"working"'*) ok "tick polled the pane option into hook_state" ;;
    *) bad "hook_state not updated by the headless poll" ;;
  esac
  # Steady state must stay silent: a second tick re-reads the same option but
  # must not error or change the value (dedup against the stored state).
  e2e_cli automation tick >/dev/null || die "second automation tick failed"
  case "$(e2e_cli session list)" in
    *'"hook_state":"working"'*) ok "second tick is a stable no-op" ;;
    *) bad "hook_state lost after a steady-state tick" ;;
  esac

  shared_sessions_probe

  if [ "$FAILS" -gt 0 ]; then
    fail "remote hook provisioning / shared sessions checks failed ($FAILS)"
    return 1
  fi
  e2e_assert "ssh:podman" "$backend" "$remote_window" "$remote_wt" \
    "remote SSH session created on the container (+ hook provisioning verified)" \
    "expected ssh:podman + remote window + remote worktree"
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
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
