#!/usr/bin/env sh
set -e

# Thurbox Installation Script
# Usage: curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh

REPO="${REPO:-Thurbeen/thurbox}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${VERSION:-}"
TEMP_DIR=""

# Cleanup
cleanup() {
  [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ] && rm -rf "$TEMP_DIR"
}
trap cleanup EXIT INT TERM

# Logging
info() { echo "→ $*" >&2; }
error() { echo "✗ Error: $*" >&2; }
success() { echo "✓ $*" >&2; }

# Detect platform
detect_platform() {
  local os=$(uname -s)
  local arch=$(uname -m)

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

# Map platform to Rust target
get_target() {
  case "$1" in
    linux-x86_64) echo "x86_64-unknown-linux-musl" ;;
    linux-aarch64) echo "aarch64-unknown-linux-gnu" ;;
    darwin-x86_64) echo "x86_64-apple-darwin" ;;
    darwin-aarch64) echo "aarch64-apple-darwin" ;;
    *) error "Unsupported platform: $1"; return 1 ;;
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
  local response=$(fetch_url "https://api.github.com/repos/${REPO}/releases/latest")
  local v=$(echo "$response" | grep -o '"tag_name":"[^"]*' | head -1 | cut -d'"' -f4)
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
  local line=$(grep "thurbox.*$2" "$1" | head -1)
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
  local url_base="https://github.com/${REPO}/releases/download/${version}"

  info "Downloading checksums..."
  download "${url_base}/thurbox-${version}-checksums.txt" "$tmpdir/checksums.txt" || {
    error "Binaries not ready. Check: https://github.com/${REPO}/releases/tag/${version}"
    return 1
  }

  info "Downloading binary..."
  download "${url_base}/${base}.tar.gz" "$tmpdir/binary.tar.gz" || { error "Download failed"; return 1; }

  info "Verifying checksum..."
  local sum=$(get_checksum "$tmpdir/checksums.txt" "$target") || { error "Target not found in checksums"; return 1; }
  check_sum "$tmpdir/binary.tar.gz" "$sum" || return 1

  echo "$tmpdir/binary.tar.gz"
}

# Install binary
do_install() {
  local tarball="$1" dir="$2"

  info "Installing..."
  mkdir -p "$dir"
  tar -xzf "$tarball" -C "$dir"
  chmod +x "$dir/thurbox"
}

# Show success message
show_success() {
  success "Thurbox installed to $1/thurbox"

  if ! echo "$PATH" | grep -q "$1"; then
    echo "⚠ Add to PATH: export PATH=\"$1:\$PATH\""
  fi

  echo ""
  echo "Setup:"
  echo "  • Install tmux >= 3.2"
  echo "  • Install claude CLI"
  echo "  • Run: thurbox"
}

# Main
main() {
  info "Thurbox Installer"

  local platform target version binary

  platform=$(detect_platform) || return 1
  info "Platform: $platform"

  target=$(get_target "$platform") || return 1
  info "Target: $target"

  TEMP_DIR=$(mktemp -d) || { error "Failed to create temp dir"; return 1; }

  version=$(get_version) || return 1
  info "Version: $version"

  binary=$(get_binary "$version" "$target" "$TEMP_DIR") || return 1

  do_install "$binary" "$INSTALL_DIR"

  show_success "$INSTALL_DIR"
  success "Installation complete!"
}

[ -z "$TEST_TMPDIR" ] && main "$@"
