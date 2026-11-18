#!/usr/bin/env sh
set -e

# Thurbox Installation Script
# Usage: curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
# Fleet alone, headless: ... | sh -s -- --fleet-only  (or THURBOX_FLEET_ONLY=1)

REPO="${REPO:-Thurbeen/thurbox}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${VERSION:-}"
# Where the release assets are fetched from; a mirror, or a local copy in tests.
DOWNLOAD_URL="${DOWNLOAD_URL:-https://github.com/${REPO}/releases/download}"
# Every program a v3 install runs by name, and the part Fleet runs without
# thurbox: BINARIES and FLEET_BINARIES in scripts/ci/release-archive.sh.
BINARIES="thurbox thurbox-cli fleet runtime-mux workspace-git"
FLEET_BINARIES="fleet runtime-mux workspace-git"
FLEET_ONLY="${THURBOX_FLEET_ONLY:-}"
TEMP_DIR=""

# Colors (auto-disabled when stderr is not a terminal, NO_COLOR is set, or TERM=dumb)
C_RESET=''; C_BOLD=''; C_DIM=''; C_RED=''; C_GREEN=''; C_YELLOW=''; C_CYAN=''; C_MAGENTA=''
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != "dumb" ]; then
  ESC=$(printf '\033')
  C_RESET="${ESC}[0m"; C_BOLD="${ESC}[1m"; C_DIM="${ESC}[2m"
  C_RED="${ESC}[31m"; C_GREEN="${ESC}[32m"; C_YELLOW="${ESC}[33m"
  C_CYAN="${ESC}[36m"; C_MAGENTA="${ESC}[35m"
fi

# Cleanup
cleanup() {
  [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ] && rm -rf "$TEMP_DIR" || true
}
trap cleanup EXIT INT TERM

# Logging
info() { printf '%b\n' "${C_CYAN}▸${C_RESET} $*" >&2; }
error() { printf '%b\n' "${C_RED}${C_BOLD}✗ Error:${C_RESET}${C_RED} $*${C_RESET}" >&2; }
success() { printf '%b\n' "${C_GREEN}✓${C_RESET} $*" >&2; }
warn() { printf '%b\n' "${C_YELLOW}⚠${C_RESET} $*" >&2; }
step() { printf '%b\n' "  ${C_DIM}$*${C_RESET}" >&2; }

# Thurbox ASCII art banner (doom font)
banner() {
  printf '%b' "$C_BOLD$C_MAGENTA" >&2
  # shellcheck disable=SC1003 # trailing backslashes are ASCII art, not quote escapes
  printf '%s\n' \
'   _____ _   _ _   _____________  _______   __' \
'  |_   _| | | | | | | ___ \ ___ \|  _  \ \ / /' \
'    | | | |_| | | | | |_/ / |_/ /| | | |\ V / ' \
'    | | |  _  | | | |    /| ___ \| | | |/   \ ' \
'    | | | | | | |_| | |\ \| |_/ /\ \_/ / /^\ \' \
'    \_/ \_| |_/\___/\_| \_\____/  \___/\/   \/' >&2
  printf '%b\n\n' "$C_RESET  ${C_DIM}multi-session coding-agent orchestrator${C_RESET}" >&2
}

# Detect platform
detect_platform() {
  local os="$(uname -s)"
  local arch="$(uname -m)"

  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="darwin" ;;
    *) error "Unsupported OS"; return 1 ;;
  esac

  case "$arch" in
    arm64) arch="aarch64" ;;
    x86_64|aarch64) ;;
    *) error "Unsupported arch"; return 1 ;;
  esac

  echo "${os}-${arch}"
}

# Map platform to Rust target. Restricted to the platforms the release matrix
# (cd.yml) actually builds a .tar.gz for: Linux x86_64 and aarch64 (we pick the
# portable musl tarballs) and Apple-silicon macOS. Intel macOS has no build, so
# it errors cleanly rather than 404 on a missing download.
get_target() {
  case "$1" in
    linux-x86_64) echo "x86_64-unknown-linux-musl" ;;
    linux-aarch64|linux-arm64) echo "aarch64-unknown-linux-musl" ;;
    darwin-aarch64) echo "aarch64-apple-darwin" ;;
    *) error "Unsupported platform: $1 (no release artifact is built for it)"; return 1 ;;
  esac
}

# Check if command exists
cmd_exists() { command -v "$1" > /dev/null 2>&1; }

# Fetch URL using curl or wget
fetch_url() {
  local url="$1"
  if cmd_exists curl; then
    curl -s "$url"
  elif cmd_exists wget; then
    wget -q -O - "$url"
  else
    error "curl or wget required"; return 1
  fi
}

# Download file using curl or wget
download() {
  local url="$1" output="$2"
  if cmd_exists curl; then
    curl -fsSL -o "$output" "$url"
  elif cmd_exists wget; then
    wget -q -O "$output" "$url"
  else
    error "curl or wget required"; return 1
  fi
}

# Get latest version from GitHub API or scrape releases page
get_version() {
  [ -n "$VERSION" ] && { echo "$VERSION"; return 0; }

  # Try API
  local response="$(fetch_url "https://api.github.com/repos/${REPO}/releases/latest")"
  local v="$(echo "$response" | grep -o '"tag_name": *"[^"]*' | head -1 | cut -d'"' -f4)"
  [ -n "$v" ] && { echo "$v"; return 0; }

  # Fallback: scrape releases page
  response=$(fetch_url "https://github.com/${REPO}/releases" 2>/dev/null)
  v=$(echo "$response" | grep -o 'releases/tag/v[0-9.]*' | head -1 | sed 's|releases/tag/||')
  [ -n "$v" ] && { echo "$v"; return 0; }

  error "Could not fetch version. Try: VERSION=v0.1.0 $0"
  return 1
}

# Extract checksum from file
get_checksum() {
  local line="$(grep "thurbox.*$2" "$1" | head -1)"
  [ -z "$line" ] && { error "Checksum not found for $2"; return 1; }
  echo "$line" | awk '{print $1}'
}

# Verify checksum
check_sum() {
  local file="$1" expected="$2"
  local actual

  if cmd_exists sha256sum; then
    actual=$(sha256sum "$file" | awk '{print $1}')
  elif cmd_exists shasum; then
    actual=$(shasum -a 256 "$file" | awk '{print $1}')
  else
    error "sha256sum or shasum required"
    return 1
  fi

  if [ "$actual" != "$expected" ]; then
    error "Checksum mismatch"
    return 1
  fi
}

# Download and verify binary
get_binary() {
  local version="$1" target="$2" tmpdir="$3"
  local base="thurbox-${version}-${target}"
  local url_base="${DOWNLOAD_URL}/${version}"

  # A 3.x release's sums file is thurbox-<V>-sha256sums.txt: it never ships
  # thurbox-v<V>-checksums.txt, the name every 2.x updater fetches first, so
  # those updaters fail safe on it (docs/thurbox/releasing.md, "v2 and v3").
  # A pinned 2.x release has only that old name.
  info "Downloading checksums..."
  download "${url_base}/thurbox-${version#v}-sha256sums.txt" "$tmpdir/checksums.txt" 2>/dev/null \
    || download "${url_base}/thurbox-${version}-checksums.txt" "$tmpdir/checksums.txt" || {
    error "Binaries not ready. Check: https://github.com/${REPO}/releases/tag/${version}"
    return 1
  }

  info "Downloading binary..."
  download "${url_base}/${base}.tar.gz" "$tmpdir/binary.tar.gz" || { error "Download failed"; return 1; }

  info "Verifying checksum..."
  local sum="$(get_checksum "$tmpdir/checksums.txt" "$target")" || { error "Target not found in checksums"; return 1; }
  check_sum "$tmpdir/binary.tar.gz" "$sum" || return 1

  echo "$tmpdir/binary.tar.gz"
}

do_install() {
  local tarball="$1" dir="$2" home v="${3:-unversioned}" rel old

  info "Installing..."
  mkdir -p "$dir"
  home="$(cd "$dir/.." && pwd)/lib/thurbox" rel="releases/${v#v}"
  old="$(readlink "$home/current" 2>/dev/null || true)"
  mkdir -p "$home/$rel/bin"
  if [ -n "$FLEET_ONLY" ]; then
    # shellcheck disable=SC2086 # one archive member per word
    tar -xzf "$tarball" -C "$home/$rel/bin" $BINARIES || { error "This release has no Fleet: VERSION=v3.x.y"; return 1; }
  else
    tar -xzf "$tarball" -C "$home/$rel/bin"
  fi
  [ -z "$old" ] || [ "$old" = "$rel" ] || { cp -pn "$home/$old/bin/"* "$home/$rel/bin/" 2>/dev/null || true; ln -sfn "$old" "$home/previous"; }
  ln -sfn "$rel" "$home/current"
  ln -sfn "$(cd "$dir" && pwd)" "$home/install-dir"
  for bin in $BINARIES; do
    if [ -f "$home/$rel/bin/$bin" ]; then chmod +x "$home/$rel/bin/$bin"; ln -sfn "$home/current/bin/$bin" "$dir/$bin"; fi
  done
}

start_service() {
  [ -z "${THURBOX_NO_SERVICE:-}" ] && [ -f "$1/fleet" ] && [ "$(uname -s)" = Linux ] \
    && systemctl --user show-environment > /dev/null 2>&1 || return 0
  if "$1/fleet" init > /dev/null && "$1/fleet" service install --now >&2; then FLEET_SERVICE=1
  else warn "Fleet's user service did not start: ${C_BOLD}fleet service install --now${C_RESET}"; fi
}

# Show success message
show_success() {
  printf '\n' >&2
  success "$([ -n "$FLEET_ONLY" ] && echo Fleet || echo Thurbox) installed to ${C_BOLD}$1${C_RESET}"
  for bin in $BINARIES; do
    if [ -f "$1/$bin" ]; then step "  $bin"; fi
  done

  if ! echo "$PATH" | grep -q "$1"; then
    warn "Add to PATH: ${C_BOLD}export PATH=\"$1:\$PATH\"${C_RESET}"
  fi

  printf '\n%b\n' "${C_BOLD}${C_MAGENTA}Next steps${C_RESET}" >&2
  step "• Install tmux >= 3.2"
  step "• Install a coding-agent CLI (claude, codex, antigravity, opencode, aider, …)"
  if [ -n "${FLEET_SERVICE:-}" ]; then
    step "• Fleet runs as a user service: ${C_CYAN}fleet service status${C_RESET}"
  elif [ -n "$FLEET_ONLY" ]; then
    step "• Set up Fleet:      ${C_CYAN}fleet init${C_RESET}"
    step "• Run it:            ${C_CYAN}fleet up${C_RESET}"
  fi
  if [ -n "$FLEET_ONLY" ]; then
    step "• Check the setup:   ${C_CYAN}fleet doctor${C_RESET}"
    step "• Hand it a Task:    ${C_CYAN}fleet apply -f task.json${C_RESET}"
    # A shell that kept the variable from an earlier install must not skip thurbox silently.
    [ -n "${FLEET_FLAG:-}" ] || warn "THURBOX_FLEET_ONLY is set, so thurbox was not installed. Unset it for a full install."
    return
  fi
  if [ -f "$1/fleet" ]; then
    [ -n "${FLEET_SERVICE:-}" ] || step "• Set up Fleet:      ${C_CYAN}fleet init${C_RESET}"
  else
    warn "This release has no Fleet. thurbox v3 needs a 3.x release: VERSION=v3.x.y"
  fi
  step "• Check the setup:   ${C_CYAN}thurbox-cli doctor${C_RESET}"
  step "• Launch the TUI:    ${C_CYAN}thurbox${C_RESET}"
}

# Main
main() {
  for arg in "$@"; do
    case "$arg" in
      --fleet-only) FLEET_ONLY=1 FLEET_FLAG=1 ;;
      *) error "Unknown option: $arg (the one option is --fleet-only)"; return 1 ;;
    esac
  done
  [ "$FLEET_ONLY" = 0 ] && FLEET_ONLY=""
  [ -n "$FLEET_ONLY" ] && BINARIES="$FLEET_BINARIES"
  banner

  local platform target version binary

  platform=$(detect_platform) || return 1
  info "Platform: ${C_BOLD}$platform${C_RESET}"

  target=$(get_target "$platform") || return 1
  info "Target:   ${C_BOLD}$target${C_RESET}"

  TEMP_DIR=$(mktemp -d) || { error "Failed to create temp dir"; return 1; }

  version=$(get_version) || return 1
  info "Version:  ${C_BOLD}$version${C_RESET}"

  binary=$(get_binary "$version" "$target" "$TEMP_DIR") || return 1

  do_install "$binary" "$INSTALL_DIR" "$version"
  start_service "$INSTALL_DIR"

  show_success "$INSTALL_DIR"
  printf '\n' >&2
  success "${C_BOLD}Installation complete!${C_RESET} Happy hacking ${C_MAGENTA}❯_${C_RESET}"
}

if [ -z "$TEST_TMPDIR" ]; then main "$@"; fi
