#!/usr/bin/env bash
# Install all required development tools for thurbox.
#
# This is the NON-NIX fallback. The recommended path is the Nix flake, which
# pins the whole toolchain reproducibly:
#
#     nix develop            # or `direnv allow` once, then it auto-enters
#
# Use this script if you don't have Nix. It also installs the couple of tools
# not yet packaged in nixpkgs (prek, rumdl, nightly cargo-pup), so the flake's
# shellHook points here for those. See docs/DEVELOPMENT.md.

set -e

echo "Installing thurbox development tools..."
echo ""

# Check if cargo-binstall is available (faster installation)
if command -v cargo-binstall &> /dev/null; then
    echo "Using cargo-binstall for faster installation..."
    INSTALL_CMD="cargo binstall -y"
else
    echo "Using cargo install (consider installing cargo-binstall for faster installs)..."
    INSTALL_CMD="cargo install --locked"
fi

# Install stable tools
echo "📦 Installing stable Rust tools..."
$INSTALL_CMD prek
$INSTALL_CMD cocogitto
$INSTALL_CMD cargo-nextest
$INSTALL_CMD cargo-modules
$INSTALL_CMD cargo-deny
$INSTALL_CMD rumdl

# ShellCheck is not a cargo crate — install it from your package manager
if ! command -v shellcheck &> /dev/null; then
    echo ""
    echo "⚠️  shellcheck not found — install it for the pre-commit shell linter:"
    echo "     Debian/Ubuntu: sudo apt-get install shellcheck"
    echo "     macOS:         brew install shellcheck"
    echo "     Arch:          sudo pacman -S shellcheck"
fi

# Install nightly tools
echo ""
echo "📦 Installing nightly Rust tools..."
NIGHTLY_VERSION="nightly-2026-01-22"
echo "Installing specific nightly toolchain: $NIGHTLY_VERSION (required for cargo-pup)"
if ! rustup toolchain list | grep -q "$NIGHTLY_VERSION"; then
    rustup toolchain install "$NIGHTLY_VERSION"
fi
echo "Installing required rustc components for cargo-pup..."
rustup component add --toolchain "$NIGHTLY_VERSION" rust-src rustc-dev llvm-tools-preview
cargo +"$NIGHTLY_VERSION" install cargo_pup

echo ""
echo "✅ All development tools installed successfully!"
echo ""
echo "Next steps:"
echo "  1. Install git hooks: prek install"
echo "  2. Verify installation: cargo check"
