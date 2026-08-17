{
  description = "thurbox — multi-session coding-agent TUI orchestrator (dev environment)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Single source of truth for the Rust toolchain: the same
        # rust-toolchain.toml cargo/rustup already honor (stable + rustfmt,
        # clippy, rust-src). No version drift between flake users and others.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Cargo dev tools that ARE packaged in nixpkgs (pinned by flake.lock).
        cargoTools = with pkgs; [
          cargo-nextest
          cargo-deny
          cargo-audit
          cargo-modules
          cocogitto
        ];

        # System deps to build/test/lint thurbox (mirrors .github/workflows/ci.yml).
        systemTools = with pkgs; [
          tmux # session backend (CLAUDE.md: >= 3.2)
          git
          shellcheck # shell linter (pre-commit + CI)
          selene # Lua linter for ui/ (pre-commit + CI); enforces the plugin sandbox
          stylua # Lua formatter for ui/ (pre-commit + CI)
          lua-language-server # Lua type checker for ui/ (CI; `just lint`)
          bats # install-script tests
          nodejs_22 # website linters (CI uses 26; 22 runs eleventy/eslint/etc.)
          just # task runner (see justfile)
          sqlite # demo/record.sh queries the dev DB
          jq # handy for `thurbox-cli … --json` in the sandbox
        ];

        # Optional demo-recording stack (scripts/demo/record.sh).
        demoTools = with pkgs; [
          vhs
          ffmpeg
          ttyd
        ];

        # Dev tools NOT in nixpkgs — the shellHook nudges the user to install
        # them via scripts/install-dev-tools.sh (cargo-binstall). Keeping the
        # flake the single entrypoint without blocking on un-packaged tools.
        # (prek = pre-commit runner, rumdl = markdown linter, cargo-pup = nightly.)
        missingHint = ''
          for t in prek rumdl; do
            command -v "$t" >/dev/null 2>&1 || {
              echo "note: '$t' is not packaged in nixpkgs — install the remaining dev tools with:"
              echo "        scripts/install-dev-tools.sh"
              break
            }
          done
        '';
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain ] ++ cargoTools ++ systemTools ++ demoTools;

          # rust-analyzer / some tools want the std sources.
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            echo "thurbox dev shell — rust $(rustc --version | cut -d' ' -f2), tmux $(tmux -V | cut -d' ' -f2)"
            echo "  build/test/lint: just <task>   |   run isolated: scripts/dev/sandbox.sh   |   git hooks: prek install"
            ${missingHint}
          '';
        };
      }
    );
}
