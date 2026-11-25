#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/dev/e2e/lib/e2e-common.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/e2e/lib/e2e-common.sh"

WORKDIR="${THURBOX_WIN_TEST_DIR:-$REPO_ROOT/target/windows-test}"
IMAGE="docker.io/dockurr/windows"
CONTAINER="thurbox-windows"
SSH_PORT="${THURBOX_WIN_SSH_PORT:-2223}"
RDP_PORT="${THURBOX_WIN_RDP_PORT:-3389}"
WEB_PORT="${THURBOX_WIN_WEB_PORT:-8006}"
WIN_VERSION="${THURBOX_WIN_VERSION:-11}"
WIN_RAM="${THURBOX_WIN_RAM:-4G}"
WIN_CPUS="${THURBOX_WIN_CPUS:-4}"
WIN_DISK="${THURBOX_WIN_DISK:-64G}"
PSMUX_VERSION="${PSMUX_VERSION:-v3.3.6}"
PSMUX_ZIP_URL="https://github.com/psmux/psmux/releases/download/${PSMUX_VERSION}/psmux-${PSMUX_VERSION}-windows-x64.zip"
NEXTEST_VERSION="${NEXTEST_VERSION:-latest}"
NEXTEST_ZIP_URL="https://get.nexte.st/${NEXTEST_VERSION}/windows"
WIN_TARGET="x86_64-pc-windows-gnu"

KEY="$WORKDIR/id_ed25519"
OEM_DIR="$WORKDIR/oem"
STORAGE_DIR="$WORKDIR/storage"

WIN_USER="Docker"
WIN_PASS="admin"

SOCKET="thurbox-dev"

ssh_vm() {
  ssh -p "$SSH_PORT" -i "$KEY" \
    -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=6 \
    "$WIN_USER@localhost" "$@"
}

scp_vm() {
  scp -P "$SSH_PORT" -i "$KEY" \
    -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=6 \
    "$@"
}

usage() {
  cat <<'USAGE'

e2e: thurbox's Windows/psmux support against a throwaway Windows VM — the
ephemeral-Windows member of the e2e family (see scripts/dev/README.md).
Mirrors linux-container.sh: a single Podman container runs a real,
KVM-accelerated Windows VM via dockur/windows, with an unattended first-boot
script that installs psmux + OpenSSH so the harness can drive the VM headlessly
over SSH — exactly like the Linux-container test drives its container.

All state lives under target/windows-test/ (gitignored): the throwaway SSH
keypair, the cached psmux release zip, the generated /oem setup payload, and
the VM disk image. Nothing touches your real ~/.ssh or ~/.config.

Usage:
  scripts/dev/e2e/windows-vm.sh up      # build /oem payload + boot the VM (first run installs Windows, ~10-20 min)
  scripts/dev/e2e/windows-vm.sh wait     # block until the VM's SSH is reachable
  scripts/dev/e2e/windows-vm.sh test     # headless smoke test (asserts psmux + a control-mode session round-trip)
  scripts/dev/e2e/windows-vm.sh test-suite # run the FULL nextest suite inside the VM (cross-built archive)
  scripts/dev/e2e/windows-vm.sh ssh      # open a PowerShell shell inside the VM
  scripts/dev/e2e/windows-vm.sh deploy   # cross-build thurbox for Windows + copy the .exe into the VM
  scripts/dev/e2e/windows-vm.sh web      # print the browser viewer URL (eyes-on)
  scripts/dev/e2e/windows-vm.sh rdp      # print RDP connection details
  scripts/dev/e2e/windows-vm.sh logs     # follow container/install logs
  scripts/dev/e2e/windows-vm.sh down     # stop + remove the container (keeps the disk image)
  scripts/dev/e2e/windows-vm.sh clean    # remove container + all local state (disk image included)

Env overrides:
  THURBOX_WIN_SSH_PORT  (default 2223)   host port forwarded to the VM's :22
  THURBOX_WIN_RDP_PORT  (default 3389)   host port forwarded to the VM's :3389
  THURBOX_WIN_WEB_PORT  (default 8006)   host port for the browser viewer
  THURBOX_WIN_VERSION   (default 11) any dockur VERSION (11, 10, 2025, 2022, ...);
                                      note: dockur has no "tiny" edition token.
  THURBOX_WIN_RAM       (default 4G)
  THURBOX_WIN_CPUS      (default 4)
  THURBOX_WIN_DISK      (default 64G)
  THURBOX_WIN_TEST_DIR  (default <repo>/target/windows-test)
  PSMUX_VERSION         (default v3.3.6) psmux release tag to install in the VM
  NEXTEST_VERSION       (default latest) cargo-nextest release to install in the VM

Requires: podman (with /dev/kvm + /dev/net/tun), ssh, ssh-keygen, curl.
Cross-build (deploy / test-suite) additionally needs the x86_64-pc-windows-gnu
Rust target, the mingw-w64 toolchain, and (for test-suite) cargo-nextest on the
host. No Rust toolchain is needed *inside* the VM: test-suite cross-builds a
self-contained nextest archive on the host and runs it with cargo-nextest.exe.
USAGE
}

require_kvm() {
  command -v podman >/dev/null || die "podman not found"
  [ -e /dev/kvm ]     || die "/dev/kvm missing — KVM is required to run the Windows VM"
  [ -e /dev/net/tun ] || die "/dev/net/tun missing — load the 'tun' module (modprobe tun)"
}

# dockur copies /oem to C:\OEM in the VM and runs install.bat once, as admin, after Windows installs.
build_oem() {
  mkdir -p "$OEM_DIR"

  if [ ! -f "$KEY" ]; then
    log "generating throwaway keypair at $KEY"
    ssh-keygen -t ed25519 -N "" -C thurbox-windows-test -f "$KEY" >/dev/null
  fi
  cp "$KEY.pub" "$OEM_DIR/authorized_keys"

  if [ ! -f "$OEM_DIR/psmux.zip" ]; then
    log "fetching psmux $PSMUX_VERSION → $OEM_DIR/psmux.zip"
    curl -fL# -o "$OEM_DIR/psmux.zip" "$PSMUX_ZIP_URL" \
      || die "could not download psmux from $PSMUX_ZIP_URL"
  fi

  if [ ! -f "$OEM_DIR/nextest.zip" ]; then
    log "fetching cargo-nextest ($NEXTEST_VERSION) → $OEM_DIR/nextest.zip"
    curl -fL# -o "$OEM_DIR/nextest.zip" "$NEXTEST_ZIP_URL" \
      || die "could not download cargo-nextest from $NEXTEST_ZIP_URL"
  fi

  cat > "$OEM_DIR/install.bat" <<'EOF'
@echo off
echo [thurbox] running first-boot setup...
powershell -ExecutionPolicy Bypass -NoProfile -File "%~dp0setup.ps1" >> "%~dp0setup.log" 2>&1
echo [thurbox] setup exit code %errorlevel% >> "%~dp0setup.log"
EOF

  cat > "$OEM_DIR/setup.ps1" <<'EOF'
$ErrorActionPreference = 'Continue'
$oem = $PSScriptRoot
Write-Host "[thurbox] setup.ps1 starting from $oem"

# --- OpenSSH server (so the harness can drive the VM headlessly) -------------
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
New-NetFirewallRule -Name 'sshd' -DisplayName 'OpenSSH Server (sshd)' `
  -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 `
  -ErrorAction SilentlyContinue

# Use PowerShell as the SSH login shell (so `ssh vm "tmux -V"` runs in pwsh).
New-ItemProperty -Path 'HKLM:\SOFTWARE\OpenSSH' -Name DefaultShell `
  -Value 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' `
  -PropertyType String -Force | Out-Null

# Install the throwaway public key for key-based admin login. Windows OpenSSH
# uses a single shared file for all administrators, with strict ACLs.
$adminKeys = "$env:ProgramData\ssh\administrators_authorized_keys"
Copy-Item "$oem\authorized_keys" $adminKeys -Force
icacls $adminKeys /inheritance:r | Out-Null
icacls $adminKeys /grant 'Administrators:F' /grant 'SYSTEM:F' | Out-Null

# --- psmux (the Windows tmux that runtime-mux's psmux adapter invokes) -------
$tools = 'C:\Tools\psmux'
New-Item -ItemType Directory -Force -Path $tools | Out-Null
Expand-Archive -Path "$oem\psmux.zip" -DestinationPath $tools -Force
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if ($machinePath -notlike "*$tools*") {
  [Environment]::SetEnvironmentVariable('Path', "$machinePath;$tools", 'Machine')
}

# --- cargo-nextest (runs the cross-built test archive; no Rust toolchain) -----
if (Test-Path "$oem\nextest.zip") {
  $ntdir = 'C:\Tools\nextest'
  New-Item -ItemType Directory -Force -Path $ntdir | Out-Null
  Expand-Archive -Path "$oem\nextest.zip" -DestinationPath $ntdir -Force
  $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  if ($machinePath -notlike "*$ntdir*") {
    [Environment]::SetEnvironmentVariable('Path', "$machinePath;$ntdir", 'Machine')
  }

  # cargo-nextest.exe is an MSVC build — it needs the VC++ runtime
  # (vcruntime140.dll), which a bare Windows image lacks (it fails to start with
  # 0xC0000135 DLL_NOT_FOUND otherwise). Install the redistributable.
  if (-not (Test-Path 'C:\Windows\System32\vcruntime140.dll')) {
    try {
      $vcr = "$env:TEMP\vc_redist.x64.exe"
      Invoke-WebRequest 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $vcr -UseBasicParsing
      Start-Process $vcr -ArgumentList '/install','/quiet','/norestart' -Wait
    } catch {
      Write-Host "[thurbox] VC++ redist install failed: $_"
    }
  }
}

# --- git (thurbox needs it for worktrees); best-effort via winget ------------
try {
  winget install --id Git.Git -e --silent --accept-source-agreements --accept-package-agreements
} catch {
  Write-Host "[thurbox] winget git install skipped: $_"
}

New-Item -ItemType File -Force -Path 'C:\thurbox-oem-done.txt' `
  -Value (Get-Date -Format o) | Out-Null
Write-Host "[thurbox] setup.ps1 finished"
EOF

  log "/oem payload ready in $OEM_DIR"
}

cmd_up() {
  require_kvm
  mkdir -p "$STORAGE_DIR"
  # dockur: Windows Setup can fail on a copy-on-write filesystem such as btrfs.
  if command -v chattr >/dev/null 2>&1; then
    chattr +C "$STORAGE_DIR" >/dev/null 2>&1 || true
  fi
  build_oem

  podman rm -f "$CONTAINER" >/dev/null 2>&1 || true

  log "booting Windows VM ($WIN_VERSION, $WIN_RAM RAM, $WIN_CPUS CPUs, $WIN_DISK disk)"
  log "first boot installs Windows unattended — watch progress at http://localhost:$WEB_PORT"
  # dockur forwards only 3389 to a Windows guest unless USER_PORTS names more.
  podman run -d --name "$CONTAINER" \
    --device=/dev/kvm --device=/dev/net/tun --cap-add NET_ADMIN \
    -e "VERSION=$WIN_VERSION" \
    -e "RAM_SIZE=$WIN_RAM" -e "CPU_CORES=$WIN_CPUS" -e "DISK_SIZE=$WIN_DISK" \
    -e "USERNAME=$WIN_USER" -e "PASSWORD=$WIN_PASS" \
    -e "USER_PORTS=22" \
    -p "$WEB_PORT:8006" \
    -p "$RDP_PORT:3389/tcp" -p "$RDP_PORT:3389/udp" \
    -p "$SSH_PORT:22/tcp" \
    -v "$STORAGE_DIR:/storage" \
    -v "$OEM_DIR:/oem" \
    "$IMAGE" >/dev/null

  echo
  log "VM container started. Next:"
  printf '  watch install:  http://localhost:%s   (or: %s logs)\n' "$WEB_PORT" "$0"
  printf '  wait for SSH:   %s wait\n' "$0"
  printf '  smoke test:     %s test\n' "$0"
}

cmd_wait() {
  [ -f "$KEY" ] || die "no keypair yet — run '$0 up' first"
  podman inspect "$CONTAINER" >/dev/null 2>&1 || die "container not running — run '$0 up' first"
  local poll_attempts_10s_apart="${1:-180}"
  log "waiting for the VM's SSH to come up (Windows is installing; up to ~30 min)..."
  for i in $(seq 1 "$poll_attempts_10s_apart"); do
    if ssh_vm 'echo ok' >/dev/null 2>&1; then
      log "SSH reachable after ~$((i * 10))s"
      ssh_vm 'powershell -NoProfile -Command "Test-Path C:\thurbox-oem-done.txt"' 2>/dev/null \
        | grep -qi true && log "first-boot setup completed (psmux + OpenSSH installed)" \
        || warn "SSH is up but first-boot setup marker not found yet — psmux may still be installing"
      return 0
    fi
    sleep 10
    [ $((i % 6)) -eq 0 ] && log "  ...still installing ($((i * 10))s elapsed)"
  done
  die "VM SSH did not come up in time — check '$0 logs' / http://localhost:$WEB_PORT"
}

cmd_ssh() {
  [ -f "$KEY" ] || die "no keypair yet — run '$0 up' first"
  if [ "$#" -gt 0 ]; then ssh_vm "$@"; else ssh_vm; fi
}

PSMUX_GATE_UNPROBED="claude's forward-slash --settings path on Windows"

# shellcheck disable=SC2120
psmux_hook_gate() {
  local src="${1:-$REPO_ROOT/crates/runtime-mux/src/psmux.rs}" line
  line="$(grep -E '^[[:space:]]*const HOOK_STATUS: bool = (true|false);' "$src" 2>/dev/null)" \
    || return 1
  case "$line" in
    *'= true;')  printf 'open\n' ;;
    *'= false;') printf 'closed\n' ;;
    *)           return 1 ;;
  esac
}

# MUST KILL: rename — a name saying a value that also shows on another pane reads `no`
pane_option_measured() {
  local pane="$1" value="$2" out="$3" line found=no
  [ -n "$out" ] || { printf 'unknown\n'; return 0; }
  while IFS= read -r line; do
    case "$line" in
      "$pane $value") found=yes ;;
      *" $value")     printf 'no\n'; return 0 ;;
    esac
  done <<EOF
$out
EOF
  printf '%s\n' "$found"
}

psmux_gate_verdict() {
  local gate="$1" a="$2" b="$3"
  if [ "$a" = unknown ] || [ "$b" = unknown ]; then
    bad "psmux hook gate: nothing measured (A=$a B=$b) — the probes prove nothing either way"
  elif [ "$gate" = open ] && [ "$a$b" = yesyes ]; then
    ok "psmux pane-option mailbox: both halves hold, per pane, with the hook gate open"
  elif [ "$gate" = open ]; then
    bad "psmux hook gate: the psmux adapter reports a status channel but a half is missing (A=$a B=$b) — remote hook state is written into a mailbox psmux drops (#1170)"
  elif [ "$a$b" = yesyes ]; then
    bad "psmux hook gate: psmux implements both mailbox halves now, per pane, but the psmux adapter still reports no status channel — reconsider the gate; its remaining condition (claude's forward-slash --settings path on Windows) is not probed here (#1170)"
  else
    info "psmux hook gate stays closed, as recorded: A=$a B=$b — the transport is deferred (#1170)"
  fi
  if [ "$a$b" = yesyes ]; then
    info "psmux hook gate: $PSMUX_GATE_UNPROBED is not probed here — the mailbox holding is not the gate being proven"
  fi
  return 0
}

smoke_verdict() {
  local sessions="$1" fails="$2"
  if ! printf '%s\n' "$sessions" | grep -qx smoke; then
    fail "could not create/list a psmux session (got: ${sessions:-<none>})"
  elif [ "$fails" -gt 0 ]; then
    fail "psmux session round-tripped, but $fails probe check(s) disagree with runtime-mux's psmux gate — see the miss lines above"
  else
    pass "psmux is installed and a -L $SOCKET session round-tripped"
  fi
}

add_second_pane_to_expose_option_scope_leaks() {
  ssh_vm "psmux -L $SOCKET split-window -d -t $1" >/dev/null 2>&1 || true
}

cmd_test() {
  ssh_vm 'echo ok' >/dev/null 2>&1 || die "VM not reachable over SSH — run '$0 wait' first"

  local FAILS=0

  log "checking psmux is installed and control-mode capable"
  local ver sessions
  ver="$(ssh_vm 'psmux -V' 2>/dev/null | tr -d '\r')" \
    || die "psmux not found on PATH inside the VM"
  log "psmux reports: $ver"

  # psmux closes a session whose command exits, so the session runs the default shell.
  ssh_vm "psmux -L $SOCKET kill-server 2>\$null; psmux -L $SOCKET new-session -d -s smoke" >/dev/null 2>&1 || true
  sessions="$(ssh_vm "psmux -L $SOCKET list-sessions -F '#{session_name}'" 2>/dev/null | tr -d '\r')"
  ssh_vm "psmux -L $SOCKET kill-server" >/dev/null 2>&1 || true

  log "probing psmux pane-user-option support (hook-status gate)"
  local gate pane opt inpane measured_a=unknown measured_b=unknown
  gate="$(psmux_hook_gate)" \
    || die "could not read HOOK_STATUS from crates/runtime-mux/src/psmux.rs — the gate probes have nothing to check against"
  ssh_vm "psmux -L $SOCKET new-session -d -s probe" >/dev/null 2>&1 || true
  pane="$(ssh_vm "psmux -L $SOCKET list-panes -s -t probe -F '#{pane_id}'" 2>/dev/null | tr -d '\r' | head -n1)"
  add_second_pane_to_expose_option_scope_leaks probe
  if [ -n "$pane" ]; then
    ssh_vm "psmux -L $SOCKET set-option -p -t $pane @thurboxprobe working" >/dev/null 2>&1 || true
    opt="$(ssh_vm "psmux -L $SOCKET list-panes -s -t probe -F '#{pane_id} #{@thurboxprobe}'" 2>/dev/null | tr -d '\r')"
    measured_a="$(pane_option_measured "$pane" 'working' "$opt")"
    info "probe A (set-option -p, then #{@opt} in list-panes -F): $measured_a (got: ${opt:-<none>})"
    ssh_vm "psmux -L $SOCKET send-keys -t $pane 'psmux -L $SOCKET set-option -p @thurboxinpane done' Enter" >/dev/null 2>&1 || true
    sleep 2
    inpane="$(ssh_vm "psmux -L $SOCKET list-panes -s -t probe -F '#{pane_id} #{@thurboxinpane}'" 2>/dev/null | tr -d '\r')"
    measured_b="$(pane_option_measured "$pane" 'done' "$inpane")"
    info "probe B (id-less in-pane set-option -p on the calling pane): $measured_b (got: ${inpane:-<none>})"
  else
    info "probes A and B: could not resolve a probe pane id"
  fi
  psmux_gate_verdict "$gate" "$measured_a" "$measured_b"
  ssh_vm "psmux -L $SOCKET kill-server" >/dev/null 2>&1 || true

  # psmux #560: a multi-line payload can be cut on the wire and its tail run as a psmux command.
  log "probing psmux send-paste multi-line delivery (paste path evidence)"
  local ppane pcontent pname
  local payload
  payload="$(printf 'PASTE_HEAD_916\nrename-window pastePWNED' | base64 | tr -d '\n')"
  ssh_vm "psmux -L $SOCKET new-session -d -s pasteprobe" >/dev/null 2>&1 || true
  ppane="$(ssh_vm "psmux -L $SOCKET list-panes -s -t pasteprobe -F '#{pane_id}'" 2>/dev/null | tr -d '\r' | head -n1)"
  if [ -n "$ppane" ]; then
    ssh_vm "psmux -L $SOCKET send-paste -t $ppane $payload" >/dev/null 2>&1 || true
    sleep 2
    pcontent="$(ssh_vm "psmux -L $SOCKET capture-pane -p -t $ppane" 2>/dev/null | tr -d '\r')"
    pname="$(ssh_vm "psmux -L $SOCKET display-message -p -t pasteprobe '#{window_name}'" 2>/dev/null | tr -d '\r')"
    case "$pcontent" in
      *PASTE_HEAD_916*) ok "probe C: send-paste delivers a base64 payload into the pane" ;;
      *) info "probe C: send-paste delivered nothing (got: ${pcontent:-<none>})" ;;
    esac
    case "$pname" in
      *pastePWNED*) info "probe C: the payload tail RAN as a psmux command (psmux #560 reaches send-paste too)" ;;
      *) ok "probe C: a multi-line payload executes nothing (no wire-cut injection)" ;;
    esac
  else
    info "paste probe skipped: could not resolve a probe pane id"
  fi
  ssh_vm "psmux -L $SOCKET kill-server" >/dev/null 2>&1 || true

  # psmux drops a send-keys argument starting with `-` as an unknown flag (#920).
  log "probing psmux literal-hyphen delivery (keystroke encoding evidence)"
  local hpane hcontent
  ssh_vm "psmux -L $SOCKET new-session -d -s hyphenprobe" >/dev/null 2>&1 || true
  hpane="$(ssh_vm "psmux -L $SOCKET list-panes -s -t hyphenprobe -F '#{pane_id}'" 2>/dev/null | tr -d '\r' | head -n1)"
  if [ -n "$hpane" ]; then
    ssh_vm "psmux -L $SOCKET send-keys -t $hpane -l -N 1 'HYPA' '-'" >/dev/null 2>&1 || true
    ssh_vm "psmux -L $SOCKET send-keys -t $hpane -l -N 1 'HYPB' '0x2d'" >/dev/null 2>&1 || true
    sleep 2
    hcontent="$(ssh_vm "psmux -L $SOCKET capture-pane -p -t $hpane" 2>/dev/null | tr -d '\r')"
    case "$hcontent" in
      *HYPA-*) info "probe D: a quoted literal '-' survives — the #920 escape is no longer needed" ;;
      *HYPA*) ok "probe D: a quoted literal '-' is dropped as a flag (the #920 bug, as encoded for)" ;;
      *) info "probe D: nothing delivered (got: ${hcontent:-<none>})" ;;
    esac
    case "$hcontent" in
      *HYPB-*) ok "probe D: the 0xNN escape delivers a hyphen into the pane" ;;
      *) info "probe D: the 0xNN escape delivered no hyphen (got: ${hcontent:-<none>})" ;;
    esac
  else
    info "hyphen probe skipped: could not resolve a probe pane id"
  fi
  ssh_vm "psmux -L $SOCKET kill-server" >/dev/null 2>&1 || true

  echo
  smoke_verdict "$sessions" "$FAILS"
}

cmd_deploy() {
  ssh_vm 'echo ok' >/dev/null 2>&1 || die "VM not reachable over SSH — run '$0 wait' first"
  local target="x86_64-pc-windows-gnu"
  rustup target list --installed 2>/dev/null | grep -qx "$target" \
    || die "missing Rust target $target — run: rustup target add $target (and install mingw-w64)"

  log "cross-building thurbox + thurbox-cli for $target"
  ( cd "$REPO_ROOT" && cargo build --release --target "$target" --bin thurbox --bin thurbox-cli )

  local bindir="$REPO_ROOT/target/$target/release"
  log "copying binaries into the VM (C:\\Tools\\thurbox)"
  ssh_vm 'powershell -NoProfile -Command "New-Item -ItemType Directory -Force -Path C:\Tools\thurbox | Out-Null"' >/dev/null
  scp_vm "$bindir/thurbox.exe" "$bindir/thurbox-cli.exe" "$WIN_USER@localhost:C:/Tools/thurbox/"

  echo
  printf '\033[1;32mdeployed\033[0m  run it with:  %s ssh\n' "$0"
  printf '  then inside the VM:  C:\\Tools\\thurbox\\thurbox.exe\n'
}

cmd_test_suite() {
  ssh_vm 'echo ok' >/dev/null 2>&1 || die "VM not reachable over SSH — run '$0 wait' first"
  ssh_vm 'cargo-nextest --version' >/dev/null 2>&1 \
    || die "cargo-nextest not found in the VM — re-run '$0 up' to reinstall the /oem payload"
  ( cd "$REPO_ROOT" && cargo nextest --version ) >/dev/null 2>&1 \
    || die "cargo-nextest not installed on host — install it: cargo install cargo-nextest --locked"
  rustup target list --installed 2>/dev/null | grep -qx "$WIN_TARGET" \
    || die "missing Rust target $WIN_TARGET — run: rustup target add $WIN_TARGET (and install mingw-w64)"

  local archive="$WORKDIR/nextest-archive.tar.zst"
  local src_tar="$WORKDIR/src.tar"

  log "cross-building the test suite into a nextest archive for $WIN_TARGET"
  ( cd "$REPO_ROOT" && cargo nextest archive --workspace --target "$WIN_TARGET" \
      --archive-file "$archive" )

  log "packing the working tree (for snapshot/fixture resolution)"
  tar --exclude=./target --exclude=./.git -cf "$src_tar" -C "$REPO_ROOT" .

  log "shipping archive + sources into the VM"
  ssh_vm 'powershell -NoProfile -Command "Remove-Item -Recurse -Force C:\thurbox-tests -ErrorAction SilentlyContinue; New-Item -ItemType Directory -Force -Path C:\thurbox-tests\src | Out-Null"' >/dev/null
  scp_vm "$archive" "$WIN_USER@localhost:C:/thurbox-tests/nextest-archive.tar.zst"
  scp_vm "$src_tar" "$WIN_USER@localhost:C:/thurbox-tests/src.tar"
  ssh_vm 'tar -xf C:/thurbox-tests/src.tar -C C:/thurbox-tests/src' \
    || die "failed to extract sources in the VM"

  log "running the full suite in the VM (cargo-nextest from archive)"
  echo
  # A standalone cargo-nextest binary needs the `nextest` subcommand token cargo would inject.
  local skip_tests_reading_build_machine_sources='not package(arch-rules) and not binary(proptest_seeds)'
  # shellcheck disable=SC2016
  ssh_vm '$env:INSTA_WORKSPACE_ROOT="C:/thurbox-tests/src"; cargo-nextest nextest run --archive-file C:/thurbox-tests/nextest-archive.tar.zst --workspace-remap C:/thurbox-tests/src -E "'"$skip_tests_reading_build_machine_sources"'"'
}

cmd_web()  { printf 'Browser viewer: http://localhost:%s\n' "$WEB_PORT"; }
cmd_rdp()  { printf 'RDP: localhost:%s   user: %s   pass: %s\n' "$RDP_PORT" "$WIN_USER" "$WIN_PASS"; }
cmd_logs() { podman logs -f "$CONTAINER"; }

cmd_down() {
  podman rm -f "$CONTAINER" >/dev/null 2>&1 \
    && log "removed container $CONTAINER (disk image kept under $STORAGE_DIR)" \
    || log "no container to remove"
}

cmd_clean() {
  cmd_down
  rm -rf "$WORKDIR"
  log "removed $WORKDIR (keypair, psmux cache, VM disk image)"
}

[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

case "${1:-}" in
  up)     cmd_up ;;
  wait)   shift; cmd_wait "$@" ;;
  test)   cmd_test ;;
  test-suite) cmd_test_suite ;;
  ssh)    shift; cmd_ssh "$@" ;;
  deploy) cmd_deploy ;;
  web)    cmd_web ;;
  rdp)    cmd_rdp ;;
  logs)   cmd_logs ;;
  down)   cmd_down ;;
  clean)  cmd_clean ;;
  *) usage; exit 1 ;;
esac
