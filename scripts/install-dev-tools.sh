#!/usr/bin/env bash
set -e

echo "Installing thurbox development tools..."
echo ""

if command -v cargo-binstall &> /dev/null; then
    echo "Using cargo-binstall for faster installation..."
    INSTALL_CMD="cargo binstall -y"
else
    echo "Using cargo install (consider installing cargo-binstall for faster installs)..."
    INSTALL_CMD="cargo install --locked"
fi

echo "📦 Installing stable Rust tools..."
$INSTALL_CMD prek
$INSTALL_CMD cocogitto
$INSTALL_CMD cargo-nextest
$INSTALL_CMD cargo-deny
$INSTALL_CMD "rumdl@$("$(dirname "$0")/ci/check-rumdl-version.sh" --print)"
$INSTALL_CMD selene
$INSTALL_CMD stylua

if ! command -v lua-language-server &> /dev/null; then
    echo ""
    echo "⚠️  lua-language-server not found — install it for the Lua type check:"
    echo "     Nix:           it is in the flake's devShell already"
    echo "     Debian/Ubuntu: sudo apt-get install lua-language-server"
    echo "     macOS:         brew install lua-language-server"
    echo "     Arch:          sudo pacman -S lua-language-server"
    echo "     Otherwise:     https://github.com/LuaLS/lua-language-server/releases"
fi

if command -v rustup &> /dev/null; then
    rustup target add x86_64-pc-windows-gnu
fi
if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
    echo ""
    echo "⚠️  x86_64-w64-mingw32-gcc not found — install it for \`just clippy-windows\`:"
    echo "     Debian/Ubuntu: sudo apt-get install gcc-mingw-w64-x86-64"
    echo "     macOS:         brew install mingw-w64"
    echo "     Arch:          sudo pacman -S mingw-w64-gcc"
fi

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
