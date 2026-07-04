#!/usr/bin/env bash
#
# e2e against a REAL machine over SSH instead of the throwaway container/VM of
# linux-container.sh and windows-vm.sh — the real-host member of the e2e family
# (see scripts/dev/README.md). Linux and Windows hosts are auto-detected.
#
# Safe for machines that also run regular thurbox sessions: the e2e test uses a
# private multiplexer socket (thurbox-lab-test), keeps ALL remote state under
# one scoped directory, and runs the local CLI against an isolated
# THURBOX_CONFIG_DIR/THURBOX_DATA_DIR — your real config/sessions and the
# host's default tmux/psmux server are never touched.
#
# Usage:
#   scripts/dev/e2e/real-host.sh <host> <verb> [args...]
#
#   <host> is a ~/.ssh/config alias or user@address; auth comes entirely from
#   your SSH setup (keys/agent) — the script never handles credentials.
#
# Verbs (any host):
#   check           readiness probe: OS, git, tmux/psmux, agent CLI, WSL state
#   hosts [name]    print the hosts.toml [[hosts]] block for this machine
#   test            headless e2e: create an ssh:<name> session in a scoped
#                   test repo on the machine, assert it landed there, clean up
#   tui [name]      add the host to the persistent "lab" dev sandbox profile
#                   and launch the TUI for manual testing
#   ssh [cmd...]    interactive shell (or run a command) on the host
#   clean           remove everything `test` may have left on the host
#
# Verbs (Windows hosts):
#   deploy          cross-build thurbox/thurbox-cli (x86_64-pc-windows-gnu) and
#                   install them to C:\Tools\thurbox (added to the machine PATH)
#   run             run the deployed thurbox.exe interactively over `ssh -t`
#                   (native-Windows manual testing; WSL distros appear in its
#                   host picker)
#   native-test [agent]  headless e2e of the DEPLOYED binaries on the host:
#                   thurbox-cli.exe creates a local psmux session (agent argv +
#                   THURBOX_* env asserted intact) and thurbox.exe boots to a
#                   frame that shows it. Scoped via THURBOX_SOCKET (needs a
#                   deploy of a build that supports it). Default agent: claude
#   test-suite      run the full nextest suite on the host (cross-built
#                   archive; installs cargo-nextest.exe there if missing)
#   wsl-setup [distro]   install WSL + <distro> (default Debian) and provision
#                        it as a thurbox target: git/tmux/claude + a non-root
#                        default user (Debian/Ubuntu-family distros only)
#   wsl-check [distro]   verify the distro is thurbox-ready
#
# Env overrides:
#   THURBOX_LAB_DIR      remote scoped state dir (default ~/thurbox-lab-test on
#                        Linux, C:/thurbox-lab-test on Windows). Must contain
#                        "thurbox-lab" — `test`/`clean` rm -rf this path.
#   THURBOX_WSL_DISTRO   default distro for the wsl-* verbs (default: Debian)
#
# Requires: ssh/scp; `test`/`tui` need cargo; `deploy`/`test-suite` need the
# x86_64-pc-windows-gnu target + mingw-w64 (test-suite also needs cargo-nextest
# on this machine). JSON is parsed in-shell — no python3.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
# shellcheck source=scripts/dev/e2e/lib/e2e-common.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/e2e/lib/e2e-common.sh"

WORKDIR="$REPO_ROOT/target/lab-test"
WSL_DISTRO="${THURBOX_WSL_DISTRO:-Debian}"
WIN_TARGET="x86_64-pc-windows-gnu"
NEXTEST_VERSION="${NEXTEST_VERSION:-latest}"
NEXTEST_ZIP_URL="https://get.nexte.st/${NEXTEST_VERSION}/windows"

# Everything the e2e test creates on the host is scoped to this socket/session
# name and to LAB_DIR — private to this script, so killing/removing it can
# never reach a real thurbox (release socket "thurbox") or a dev sandbox
# session (socket "thurbox-dev") on the same machine.
LAB_SOCKET="thurbox-lab-test"
E2E_NAME="lab-e2e"
E2E_BRANCH="test/lab-e2e"
# ssh_opts for a regular/manual hosts.toml block (default socket + session, so
# sessions land where a normal thurbox on this machine expects them).
LAB_SSH_OPTS='["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]'

usage() { awk 'NR > 1 && !/^#/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"; }

DEST="${1:-}"
VERB="${2:-}"
if [ -z "$DEST" ] || [ -z "$VERB" ]; then usage; exit 1; fi
shift 2

mkdir -p "$WORKDIR"

# Control sockets live in a short runtime dir, NOT under target/ — AF_UNIX
# socket paths are limited to ~104 bytes and a deep checkout blows past that
# (same constraint as the sandbox tmux sockets in lib/sandbox-env.sh).
CM_DIR="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/thurbox-lab-cm"
mkdir -p "$CM_DIR"

# One multiplexed connection for the many small remote calls below.
SSH_OPTS=(
  -o ControlMaster=auto -o "ControlPath=$CM_DIR/%C" -o ControlPersist=60
  -o ConnectTimeout=8
)
# shellcheck disable=SC2029 # client-side expansion builds the remote command
ssh_host() { ssh "${SSH_OPTS[@]}" "$DEST" "$@"; }
scp_host() { scp -q "${SSH_OPTS[@]}" "$@"; }
# Probe-style query: remote stdout, CR-stripped, empty (not fatal) on failure.
remote_out() { ssh_host "$@" 2> /dev/null | tr -d '\r' || true; }

# ---- host detection ------------------------------------------------------

HOST_OS="" # linux | windows
LAB_DIR=""
MUX=""

detect_os() {
  [ -n "$HOST_OS" ] && return 0
  local probe
  # Keep stderr visible: ssh's own error (auth, DNS, ControlPath, …) is the
  # only clue when the probe fails.
  probe="$(ssh_host 'echo thurbox-probe' | tr -d '\r' || true)"
  [ "$probe" = "thurbox-probe" ] || die "cannot reach '$DEST' over SSH"
  local u
  u="$(remote_out 'uname -s')"
  case "$u" in
    Linux | Darwin | *BSD*) HOST_OS=linux ;;
    *)
      ssh_host 'cmd /c ver' 2>/dev/null | grep -qi windows \
        || die "cannot determine the host OS (uname said '${u:-nothing}')"
      HOST_OS=windows
      ;;
  esac
  if [ "$HOST_OS" = windows ]; then
    LAB_DIR="${THURBOX_LAB_DIR:-C:/thurbox-lab-test}"
    MUX="psmux"
  else
    local rhome
    # shellcheck disable=SC2016 # $HOME must expand on the remote, not here
    rhome="$(ssh_host 'printf %s "$HOME"' | tr -d '\r')" \
      || die "cannot read \$HOME on $DEST"
    LAB_DIR="${THURBOX_LAB_DIR:-$rhome/thurbox-lab-test}"
    MUX="tmux"
  fi
  # `test`/`clean` rm -rf LAB_DIR on a real machine — insist on a marker so a
  # stray THURBOX_LAB_DIR override can't point them at a directory that
  # matters, and reject whitespace: the path is embedded in remote command
  # strings that would split on it.
  case "$LAB_DIR" in
    *[[:space:]]*) die "THURBOX_LAB_DIR must not contain whitespace (got: $LAB_DIR)" ;;
    *thurbox-lab*) ;;
    *) die "THURBOX_LAB_DIR must contain 'thurbox-lab' (got: $LAB_DIR)" ;;
  esac
  log "host $DEST is $HOST_OS (remote test dir: $LAB_DIR)"
}

require_windows() {
  detect_os
  [ "$HOST_OS" = windows ] || die "'$VERB' applies to Windows hosts only ($DEST is $HOST_OS)"
}

remote_mkdir() {
  if [ "$HOST_OS" = windows ]; then
    ssh_host "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force -Path '$1' | Out-Null\""
  else
    ssh_host "mkdir -p '$1'"
  fi
}

# run_ps [args...] — ship the PowerShell script on stdin to the host and run it
# there. Authoring a real .ps1 file sidesteps the bash→sshd→shell quoting maze;
# args land in the script's param() block, each single-quoted for the remote
# PowerShell (embedded quotes doubled) so values with spaces survive.
run_ps() {
  local f="$WORKDIR/remote.ps1" qargs="" a
  cat > "$f"
  remote_mkdir "$LAB_DIR"
  scp_host "$f" "$DEST:$LAB_DIR/remote.ps1"
  for a in "$@"; do qargs+=" '${a//\'/\'\'}'"; done
  ssh_host "powershell -NoProfile -ExecutionPolicy Bypass -File $LAB_DIR/remote.ps1$qargs"
}

# Default hosts.toml block name: the alias, or the address part of user@addr,
# sanitized to thurbox's name charset.
block_name() {
  local n="${1:-}"
  if [ -z "$n" ]; then
    n="${DEST##*@}"
    n="${n//[^a-zA-Z0-9_-]/-}"
  fi
  printf '%s' "$n"
}

# hosts.toml [[hosts]] block for regular/manual use, via the shared emitter.
lab_hosts_block() {
  local name="$1" mux=""
  [ "$HOST_OS" = windows ] && mux="psmux"
  hosts_block "$name" "$DEST" "$LAB_SSH_OPTS" "$mux"
  [ "$HOST_OS" = windows ] && \
    printf '# worktrees_dir = "C:/Users/<user>/.local/share/thurbox/worktrees"\n'
  return 0
}

# ---- check ---------------------------------------------------------------

tmux_version_ok() { # "tmux 3.5a" / "psmux 3.3.6" → 0 if >= 3.2
  printf '%s' "$1" | awk '{ split($2, a, "."); exit !(a[1] > 3 || (a[1] == 3 && a[2] + 0 >= 2)) }'
}

cmd_check() {
  detect_os
  local FAILS=0 v
  if [ "$HOST_OS" = linux ]; then
    v="$(remote_out 'git --version')"
    [ -n "$v" ] && ok "git: $v" || bad "git not found"
    v="$(remote_out 'tmux -V')"
    if [ -n "$v" ]; then
      tmux_version_ok "$v" && ok "tmux: $v (>= 3.2)" || bad "tmux too old: $v (need >= 3.2)"
    else
      bad "tmux not found"
    fi
    # Agent CLIs live in ~/.local/bin, put on PATH by login-shell profiles —
    # the same way thurbox's remote tmux windows resolve them.
    v="$(remote_out 'bash -lc "claude --version"' | head -1)"
    [ -n "$v" ] && ok "claude: $v" || bad "claude not found via a login shell"
  else
    v="$(remote_out 'psmux -V')"
    if [ -n "$v" ]; then
      tmux_version_ok "$v" && ok "psmux: $v" || warn "psmux version parse: $v"
    else
      bad "psmux not found on PATH"
    fi
    v="$(remote_out 'git --version')"
    [ -n "$v" ] && ok "git: $v" || bad "git not found"
    v="$(remote_out 'claude --version' | head -1)"
    [ -n "$v" ] && ok "claude (Windows): $v" || info "claude not on the Windows PATH (only needed for native sessions)"
    if ssh_host 'powershell -NoProfile -Command "Test-Path C:/Tools/thurbox/thurbox.exe"' 2>/dev/null | grep -qi true; then
      ok "thurbox.exe deployed (C:/Tools/thurbox)"
    else
      info "thurbox.exe not deployed — '$0 $DEST deploy' for native/WSL manual testing"
    fi
    v="$(run_ps <<'EOF' | tr -d '\r' || true
$env:WSL_UTF8 = '1'
wsl.exe --status *> $null
if ($LASTEXITCODE -ne 0) { Write-Output 'absent'; exit 0 }
$distros = (wsl.exe -l -q) | Where-Object { $_ }
Write-Output ("present: " + ($distros -join ', '))
EOF
    )"
    case "$v" in
      present:*) ok "WSL $v" ;;
      absent*) info "WSL not installed — '$0 $DEST wsl-setup' to provision it" ;;
      *) info "WSL state unknown: $v" ;;
    esac
  fi
  echo
  if [ "$FAILS" -eq 0 ]; then
    pass "$DEST is ready as a thurbox $HOST_OS host"
  else
    fail "$FAILS missing requirement(s) — see above"
  fi
}

# ---- hosts / tui ---------------------------------------------------------

cmd_hosts() {
  detect_os
  lab_hosts_block "$(block_name "${1:-}")"
}

cmd_tui() {
  detect_os
  local name cfg
  name="$(block_name "${1:-}")"
  cfg="$REPO_ROOT/target/dev-sandbox/lab/thurbox-config"
  mkdir -p "$cfg"
  if [ -f "$cfg/hosts.toml" ] && grep -q "name = \"$name\"" "$cfg/hosts.toml"; then
    log "host '$name' already wired into the lab sandbox"
  else
    [ -f "$cfg/hosts.toml" ] || printf 'config_version = 1\n' > "$cfg/hosts.toml"
    { echo; lab_hosts_block "$name"; } >> "$cfg/hosts.toml"
    log "added host '$name' to $cfg/hosts.toml"
  fi
  log "launching the dev TUI (sandbox profile 'lab') — pick host '$name' when creating a session"
  exec "$REPO_ROOT/scripts/dev/sandbox.sh" --profile lab
}

# ---- headless e2e --------------------------------------------------------

# Tear down ONLY what the e2e test creates: its private -L server and LAB_DIR
# (repo + worktrees both live under it). Never touches other sockets.
remote_cleanup() {
  ssh_host "$MUX -L $LAB_SOCKET kill-server" > /dev/null 2>&1 || true
  if [ "$HOST_OS" = windows ]; then
    ssh_host "powershell -NoProfile -Command \"Remove-Item -Recurse -Force '$LAB_DIR' -ErrorAction SilentlyContinue\"" || true
  else
    ssh_host "rm -rf '$LAB_DIR'" || true
  fi
}

cmd_test() {
  command -v cargo > /dev/null || die "cargo not found"
  detect_os
  ssh_host 'git --version' > /dev/null 2>&1 \
    || die "git not found on $DEST — provision the host first ('$0 $DEST check')"
  if [ "$HOST_OS" = windows ]; then
    ssh_host 'psmux -V' > /dev/null 2>&1 \
      || die "psmux not found on $DEST — provision the host first ('$0 $DEST check')"
  fi

  local repo="$LAB_DIR/repo" agent_cmd="bash"
  [ "$HOST_OS" = windows ] && agent_cmd="powershell"

  log "seeding a scoped test repo at $DEST:$repo"
  remote_cleanup
  remote_mkdir "$repo"
  # Plain git invocations run identically under sh, cmd, and powershell — one
  # call per step because Windows PowerShell 5.1 has no `&&`.
  ssh_host "git -C $repo init -qb main"
  ssh_host "git -C $repo config user.email test@thurbox"
  ssh_host "git -C $repo config user.name thurbox-lab"
  ssh_host "git -C $repo commit -qm init --allow-empty"
  ssh_host "git -C $repo branch -f feature/example main"

  # Isolated local config/data (THURBOX_*_DIR, honored ahead of XDG) so the
  # e2e run never sees — or is seen by — your real thurbox database.
  local tmp
  tmp="$(mktemp -d "$WORKDIR/e2e.XXXXXX")"
  # EXIT (not RETURN) so aborts via die/set -e still tear down the remote
  # server + LAB_DIR; cmd_test is the terminal action of the dispatch.
  # shellcheck disable=SC2064 # expand now: tmp is gone when the trap fires
  trap "rm -rf '$tmp'; remote_cleanup" EXIT
  mkdir -p "$tmp/config" "$tmp/data"
  {
    echo 'config_version = 1'
    echo
    lab_hosts_block lab
    echo "socket = \"$LAB_SOCKET\""
    echo "session = \"$LAB_SOCKET\""
    echo "worktrees_dir = \"$LAB_DIR/worktrees\""
  } > "$tmp/config/hosts.toml"
  cat > "$tmp/config/agents.toml" <<EOF
default = "shell"
[[agents]]
name = "shell"
command = "$agent_cmd"
EOF

  log "creating an ssh:lab session via thurbox-cli (isolated DB)"
  export THURBOX_CONFIG_DIR="$tmp/config" THURBOX_DATA_DIR="$tmp/data"
  local result backend
  result="$(e2e_create_and_get \
    --name "$E2E_NAME" --host lab --repo-path "$repo" \
    --agent shell --worktree-branch "$E2E_BRANCH" --base-branch main)" || exit 1
  read -r _ backend <<<"$result"

  # Confirm the artifacts really live on the machine, inside our private server.
  local windows worktrees
  windows="$(ssh_host "$MUX -L $LAB_SOCKET list-windows -t $LAB_SOCKET -F \"#{window_name}\"" 2> /dev/null \
    | tr -d '\r' | grep -c "^tb-$E2E_NAME\$" || true)"
  worktrees="$(ssh_host "git -C $repo worktree list" 2> /dev/null | grep -c "$E2E_NAME" || true)"

  e2e_assert "ssh:lab" "$backend" "$windows" "$worktrees" \
    "remote SSH session created on $DEST ($HOST_OS)" \
    "expected ssh:lab + a remote window + a remote worktree"
}

cmd_clean() {
  detect_os
  remote_cleanup
  log "removed the $LAB_SOCKET server and $LAB_DIR on $DEST"
}

# ---- Windows: deploy / run / test-suite ----------------------------------

require_win_target() {
  rustup target list --installed 2> /dev/null | grep -qx "$WIN_TARGET" \
    || die "missing Rust target $WIN_TARGET — run: rustup target add $WIN_TARGET (and install mingw-w64)"
}

cmd_deploy() {
  require_windows
  require_win_target
  log "cross-building thurbox + thurbox-cli for $WIN_TARGET"
  (cd "$REPO_ROOT" && cargo build --release --target "$WIN_TARGET" --bin thurbox --bin thurbox-cli)

  log "installing to C:/Tools/thurbox (machine PATH)"
  run_ps <<'EOF'
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path 'C:/Tools/thurbox' | Out-Null
$p = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if ($p -notlike '*C:\Tools\thurbox*') {
  [Environment]::SetEnvironmentVariable('Path', "$p;C:\Tools\thurbox", 'Machine')
}
EOF
  local bindir="$REPO_ROOT/target/$WIN_TARGET/release"
  scp_host "$bindir/thurbox.exe" "$bindir/thurbox-cli.exe" "$DEST:C:/Tools/thurbox/"

  echo
  printf '\033[1;32mdeployed\033[0m  manual test:  %s %s run\n' "$0" "$DEST"
  printf '  (WSL distros on the machine appear in its host picker as wsl:<distro>)\n'
}

cmd_run() {
  require_windows
  ssh_host 'powershell -NoProfile -Command "Test-Path C:/Tools/thurbox/thurbox.exe"' 2> /dev/null \
    | grep -qi true || die "thurbox.exe not deployed — run '$0 $DEST deploy' first"
  log "running thurbox.exe on $DEST (quit the TUI to return)"
  exec ssh -t "$DEST" "C:/Tools/thurbox/thurbox.exe"
}

# Native e2e of the DEPLOYED binaries, entirely on the host: thurbox-cli.exe
# creates a local (psmux) session whose agent must come up with its argv and
# THURBOX_* env intact, then thurbox.exe boots inside a scoped psmux pane and
# must adopt that session. Fully isolated via THURBOX_SOCKET (the deployed
# build must support it) + THURBOX_CONFIG_DIR/THURBOX_DATA_DIR under LAB_DIR —
# psmux has no TMUX_TMPDIR-style socket-dir isolation, so the env override is
# the only thing keeping this off the machine's real thurbox/thurbox-dev
# servers.
cmd_native_test() {
  require_windows
  ssh_host 'powershell -NoProfile -Command "Test-Path C:/Tools/thurbox/thurbox-cli.exe"' 2> /dev/null \
    | grep -qi true || die "thurbox-cli.exe not deployed — run '$0 $DEST deploy' first"
  local agent="${1:-claude}"
  log "native e2e on $DEST: agent '$agent', scoped socket $LAB_SOCKET, state $LAB_DIR/native"
  run_ps "$LAB_DIR" "$LAB_SOCKET" "$agent" <<'EOF'
param([string]$LabDir, [string]$Socket, [string]$Agent)
$ErrorActionPreference = 'Stop'
$cli = 'C:/Tools/thurbox/thurbox-cli.exe'
$tui = 'C:/Tools/thurbox/thurbox.exe'
$name = 'lab-native'
$fails = 0
function Assert([bool]$ok, [string]$what) {
  if ($ok) { Write-Output "  ok   $what" } else { Write-Output "  FAIL $what"; $script:fails++ }
}

# Scoped env for every thurbox child below. THURBOX_SOCKET keeps the whole run
# off the machine's real psmux servers.
$env:THURBOX_SOCKET = $Socket
$env:THURBOX_CONFIG_DIR = "$LabDir/native/config"
$env:THURBOX_DATA_DIR = "$LabDir/native/data"

psmux -L $Socket kill-server 2>$null
Remove-Item -Recurse -Force "$LabDir/native" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$LabDir/native/repo" | Out-Null
git -C "$LabDir/native/repo" init -qb main
git -C "$LabDir/native/repo" config user.email test@thurbox
git -C "$LabDir/native/repo" config user.name thurbox-lab
git -C "$LabDir/native/repo" commit -qm init --allow-empty

Write-Output "== thurbox-cli session create (local psmux backend) =="
$out = & $cli --json session create --name $name --repo-path "$LabDir/native/repo" --agent $Agent 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { Write-Output $out; Write-Output "FAIL session create exited $LASTEXITCODE"; exit 1 }
$json = $out | ConvertFrom-Json
Assert ($json.name -eq $name) "session created (id $($json.id))"
Start-Sleep 8

# The scoped server (NOT thurbox-dev) must own the agent window, alive.
$panes = (psmux -L $Socket list-panes -a -F '#{window_name} #{pane_dead}') -join "`n"
Assert ($panes -match "tb-$name 0") "agent window alive on scoped socket ($Socket)"

# The window process must carry the folded env + argv (psmux drops -e and
# joined tokens; the deployed build must route around both).
$procs = (Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -match 'Set-Item Env:THURBOX_SESSION' }).CommandLine -join "`n"
Assert ($procs -match [regex]::Escape($json.id)) "THURBOX_SESSION env folded into the window command"
Assert ($procs -match "'$Agent'") "agent argv delivered ($Agent)"

$list = & $cli --json session list | Out-String | ConvertFrom-Json
Assert (@($list | Where-Object { $_.name -eq $name }).Count -eq 1) "thurbox-cli session list sees the session"

Write-Output "== thurbox.exe TUI boot (inside a scoped psmux pane) =="
# psmux targets are session-qualified; the session name is the deployed
# build's compile-time default, so discover it instead of hardcoding.
$sess = (psmux -L $Socket list-sessions -F '#{session_name}' | Select-Object -First 1)
# Window command is one argv token: env + exec. The pane gives the TUI a ConPTY.
$boot = "Set-Item Env:THURBOX_SOCKET '$Socket'; Set-Item Env:THURBOX_CONFIG_DIR '$LabDir/native/config'; Set-Item Env:THURBOX_DATA_DIR '$LabDir/native/data'; & '$tui'"
psmux -L $Socket new-window -d -t "${sess}:" -n lab-tui $boot
Start-Sleep 8
$frame = (psmux -L $Socket capture-pane -p -t "${sess}:lab-tui") -join "`n"
Assert ($frame -match 'lab-native') "TUI booted and shows/adopted the session"

psmux -L $Socket kill-server 2>$null
if ($fails -gt 0) { Write-Output "NATIVE-TEST FAILURES: $fails"; exit 1 }
Write-Output "NATIVE-TEST OK"
EOF
  pass "native thurbox (dev) runs correctly on $DEST"
}

cmd_test_suite() {
  require_windows
  require_win_target
  (cd "$REPO_ROOT" && cargo nextest --version) > /dev/null 2>&1 \
    || die "cargo-nextest not installed locally — install it: cargo install cargo-nextest --locked"

  # cargo-nextest.exe executes the cross-built archive on the host — install it
  # there on first use (plus the VC++ runtime the MSVC build needs).
  local nextest_bin="cargo-nextest"
  if ! ssh_host 'cargo-nextest --version' > /dev/null 2>&1; then
    log "installing cargo-nextest.exe on the host"
    if [ ! -f "$WORKDIR/nextest.zip" ]; then
      curl -fL# -o "$WORKDIR/nextest.zip" "$NEXTEST_ZIP_URL" \
        || die "could not download cargo-nextest from $NEXTEST_ZIP_URL"
    fi
    remote_mkdir "$LAB_DIR"
    scp_host "$WORKDIR/nextest.zip" "$DEST:$LAB_DIR/nextest.zip"
    run_ps "$LAB_DIR/nextest.zip" <<'EOF'
param([string]$Zip)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path 'C:/Tools/nextest' | Out-Null
Expand-Archive -Path $Zip -DestinationPath 'C:/Tools/nextest' -Force
$p = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if ($p -notlike '*C:\Tools\nextest*') {
  [Environment]::SetEnvironmentVariable('Path', "$p;C:\Tools\nextest", 'Machine')
}
# The MSVC-built exe needs vcruntime140.dll; a fresh Windows install lacks it.
if (-not (Test-Path 'C:\Windows\System32\vcruntime140.dll')) {
  $vcr = "$env:TEMP\vc_redist.x64.exe"
  Invoke-WebRequest 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $vcr -UseBasicParsing
  Start-Process $vcr -ArgumentList '/install', '/quiet', '/norestart' -Wait
}
EOF
    # The PATH edit only reaches future logons; this run uses the full path.
    nextest_bin="C:/Tools/nextest/cargo-nextest.exe"
  fi

  local archive="$WORKDIR/nextest-archive.tar.zst" src_tar="$WORKDIR/src.tar"
  log "cross-building the test suite into a nextest archive for $WIN_TARGET"
  (cd "$REPO_ROOT" && cargo nextest archive --workspace --target "$WIN_TARGET" --archive-file "$archive")

  log "packing the working tree (for snapshot/fixture resolution)"
  tar --exclude=./target --exclude=./.git -cf "$src_tar" -C "$REPO_ROOT" .

  log "shipping archive + sources to $DEST:$LAB_DIR/tests"
  ssh_host "powershell -NoProfile -Command \"Remove-Item -Recurse -Force '$LAB_DIR/tests' -ErrorAction SilentlyContinue; New-Item -ItemType Directory -Force -Path '$LAB_DIR/tests/src' | Out-Null\""
  scp_host "$archive" "$DEST:$LAB_DIR/tests/nextest-archive.tar.zst"
  scp_host "$src_tar" "$DEST:$LAB_DIR/tests/src.tar"
  ssh_host "tar -xf $LAB_DIR/tests/src.tar -C $LAB_DIR/tests/src" \
    || die "failed to extract sources on the host"

  log "running the full suite on $DEST"
  echo
  # INSTA_WORKSPACE_ROOT re-roots insta snapshots onto the shipped sources;
  # architecture_rules scans the source tree via a compile-time path and only
  # runs in its dedicated Linux CI job. `$env:` must reach PowerShell verbatim.
  # shellcheck disable=SC2016
  ssh_host '$env:INSTA_WORKSPACE_ROOT="'"$LAB_DIR"'/tests/src"; '"$nextest_bin"' nextest run --archive-file '"$LAB_DIR"'/tests/nextest-archive.tar.zst --workspace-remap '"$LAB_DIR"'/tests/src -E "not binary(architecture_rules)"'
}

# ---- Windows: WSL --------------------------------------------------------

cmd_wsl_setup() {
  require_windows
  local distro="${1:-$WSL_DISTRO}"
  log "provisioning WSL distro '$distro' on $DEST (best-effort; Debian/Ubuntu-family)"
  local rc=0
  run_ps "$distro" <<'EOF' || rc=$?
param([string]$Distro = 'Debian')
$ErrorActionPreference = 'Continue'
$env:WSL_UTF8 = '1'

wsl.exe --status *> $null
if ($LASTEXITCODE -ne 0) {
  Write-Output '[lab] WSL is not installed - enabling it (a reboot is usually required)'
  wsl.exe --install --no-distribution
  Write-Output '[lab] REBOOT the machine, then re-run wsl-setup.'
  exit 3
}

$distros = (wsl.exe -l -q) | Where-Object { $_ }
if ($distros -notcontains $Distro) {
  Write-Output "[lab] installing distro $Distro"
  wsl.exe --install -d $Distro --no-launch
  if ($LASTEXITCODE -ne 0) { Write-Output "[lab] distro install failed"; exit 1 }
}

Write-Output "[lab] installing git/tmux/curl inside $Distro"
wsl.exe -d $Distro -u root -- sh -c "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git tmux curl ca-certificates sudo"
if ($LASTEXITCODE -ne 0) { Write-Output '[lab] package install failed'; exit 1 }

Write-Output '[lab] ensuring a non-root default user (thurbox)'
wsl.exe -d $Distro -u root -- sh -c "id -u thurbox >/dev/null 2>&1 || useradd -m -s /bin/bash -G sudo thurbox"
wsl.exe -d $Distro -u root -- sh -c "printf '[user]\ndefault=thurbox\n' > /etc/wsl.conf"

Write-Output '[lab] installing Claude Code for the default user'
wsl.exe -d $Distro -u thurbox -- bash -c "command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash"

wsl.exe --terminate $Distro *> $null
Write-Output '[lab] done'
EOF
  if [ "$rc" -eq 3 ]; then
    warn "reboot needed: $0 $DEST ssh 'shutdown /r /t 0'  — then re-run wsl-setup"
    return 3
  fi
  [ "$rc" -eq 0 ] || return "$rc"
  log "verify with: $0 $DEST wsl-check $distro"
}

cmd_wsl_check() {
  require_windows
  local distro="${1:-$WSL_DISTRO}"
  run_ps "$distro" <<'EOF'
param([string]$Distro = 'Debian')
$ErrorActionPreference = 'Continue'
$env:WSL_UTF8 = '1'
$fails = 0

wsl.exe --status *> $null
if ($LASTEXITCODE -ne 0) { Write-Output 'miss WSL is not installed'; exit 1 }

$distros = (wsl.exe -l -q) | Where-Object { $_ }
if ($distros -notcontains $Distro) {
  Write-Output "miss distro '$Distro' not installed (have: $($distros -join ', '))"
  exit 1
}

$user = (wsl.exe -d $Distro -- whoami) -join ''
if ($user -and $user -ne 'root') { Write-Output "ok   default user: $user" }
else { Write-Output "miss default user is '$user' (want non-root)"; $fails++ }

$tmux = (wsl.exe -d $Distro -- tmux -V) -join ''
if ($tmux -match '^tmux (3\.[2-9]|[4-9])') { Write-Output "ok   $tmux (>= 3.2)" }
else { Write-Output "miss tmux missing or too old: '$tmux'"; $fails++ }

$git = (wsl.exe -d $Distro -- git --version) -join ''
if ($git) { Write-Output "ok   $git" } else { Write-Output 'miss git not found'; $fails++ }

$claude = (wsl.exe -d $Distro -- bash -lc 'claude --version 2>/dev/null') -join ''
if ($claude) { Write-Output "ok   claude: $claude" }
else { Write-Output 'miss claude not found via a login shell'; $fails++ }

if ($fails -eq 0) { Write-Output "PASS wsl:$Distro is thurbox-ready"; exit 0 }
Write-Output "FAIL $fails requirement(s) missing"
exit 1
EOF
}

# ---- ssh / dispatch ------------------------------------------------------

cmd_ssh() {
  # shellcheck disable=SC2029 # client-side expansion builds the remote command
  if [ "$#" -gt 0 ]; then ssh_host "$@"; else ssh "${SSH_OPTS[@]}" "$DEST"; fi
}

case "$VERB" in
  check)      cmd_check ;;
  hosts)      cmd_hosts "$@" ;;
  test)       cmd_test ;;
  tui)        cmd_tui "$@" ;;
  ssh)        cmd_ssh "$@" ;;
  clean)      cmd_clean ;;
  deploy)     cmd_deploy ;;
  run)        cmd_run ;;
  native-test) cmd_native_test "$@" ;;
  test-suite) cmd_test_suite ;;
  wsl-setup)  cmd_wsl_setup "$@" ;;
  wsl-check)  cmd_wsl_check "$@" ;;
  *) usage; exit 1 ;;
esac
