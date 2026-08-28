#!/usr/bin/env bash
# Install all required development tools for thurbox.
#
# This is the NON-NIX fallback. The recommended path is the Nix flake, which
# pins the whole toolchain reproducibly:
#
#     nix develop            # or `direnv allow` once, then it auto-enters
#
# Use this script if you don't have Nix. It also installs the couple of tools
# not yet packaged in nixpkgs (prek, rumdl), so the flake's
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
$INSTALL_CMD cargo-deny
$INSTALL_CMD rumdl
# The Lua interface in ui/. Both are single Rust binaries, which is why they were
# chosen over luacheck (a LuaRocks package needing a Lua toolchain).
$INSTALL_CMD selene
$INSTALL_CMD stylua

# lua-language-server is not a cargo crate either — it is how CI type-checks the
# plugin interface (`just lint`), the same tool neovim gates its own Lua on.
if ! command -v lua-language-server &> /dev/null; then
    echo ""
    echo "⚠️  lua-language-server not found — install it for the Lua type check:"
    echo "     Nix:           it is in the flake's devShell already"
    echo "     Debian/Ubuntu: sudo apt-get install lua-language-server"
    echo "     macOS:         brew install lua-language-server"
    echo "     Arch:          sudo pacman -S lua-language-server"
    echo "     Otherwise:     https://github.com/LuaLS/lua-language-server/releases"
fi

# ShellCheck is not a cargo crate — install it from your package manager
if ! command -v shellcheck &> /dev/null; then
    echo ""
    echo "⚠️  shellcheck not found — install it for the pre-commit shell linter:"
    echo "     Debian/Ubuntu: sudo apt-get install shellcheck"
    echo "     macOS:         brew install shellcheck"
    echo "     Arch:          sudo pacman -S shellcheck"
fi

echo ""
echo "✅ All development tools installed successfully!"
echo ""
echo "Next steps:"
echo "  1. Install git hooks: prek install"
echo "  2. Verify installation: cargo check"
