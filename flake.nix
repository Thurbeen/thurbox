{
  description = "thurbox — multi-session coding-agent TUI orchestrator";

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
    let
      # The flake cannot see git tags, so a build from it is never a release:
      # the base comes from Cargo.toml (`0.0.0-dev`, minus the `-dev` that would
      # make it a dev build, see nix/package.nix) and the date of the commit
      # it was built from says how new it is.
      baseVersion = nixpkgs.lib.removeSuffix "-dev" (nixpkgs.lib.importTOML ./Cargo.toml).package.version;
      date = builtins.substring 0 8 (self.lastModifiedDate or "19700101");
      version = "${baseVersion}-unstable-${builtins.substring 0 4 date}-${builtins.substring 4 2 date}-${builtins.substring 6 2 date}";

      thurboxFor =
        pkgs:
        pkgs.callPackage ./nix/package.nix {
          inherit version;
          rev = self.shortRev or self.dirtyShortRev or null;
        };

      # The modules install the package and, on request, the opt-in automation
      # timer from packaging/systemd. That timer is the only thing thurbox needs
      # a service manager for; everything else it configures itself, in files it
      # owns, so the modules manage no config.
      automationTimer = {
        OnBootSec = "1min";
        OnUnitActiveSec = "1min";
        AccuracySec = "15s";
        Persistent = true;
      };
      automationTick = package: "${package}/bin/thurbox-cli automation tick";

      options =
        { lib, pkgs, ... }:
        {
          enable = lib.mkEnableOption "thurbox, the coding-agent TUI orchestrator";
          package = lib.mkOption {
            type = lib.types.package;
            default = thurboxFor pkgs;
            defaultText = lib.literalExpression "thurbox.packages.\${pkgs.system}.default";
            description = "The thurbox package to install.";
          };
          automations.enable = lib.mkEnableOption ''
            a systemd user timer that runs `thurbox-cli automation tick` every
            minute, so automations fire after a reboot without the TUI having
            been opened
          '';
        };
    in
    {
      overlays.default = final: _prev: { thurbox = thurboxFor final; };

      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.thurbox;
        in
        {
          options.programs.thurbox = options { inherit lib pkgs; };
          config = lib.mkIf cfg.enable (
            lib.mkMerge [
              { environment.systemPackages = [ cfg.package ]; }
              (lib.mkIf cfg.automations.enable {
                systemd.user.services.thurbox-automations = {
                  description = "Fire due thurbox automations (headless)";
                  serviceConfig = {
                    Type = "oneshot";
                    ExecStart = automationTick cfg.package;
                  };
                };
                systemd.user.timers.thurbox-automations = {
                  description = "Fire due thurbox automations every minute";
                  timerConfig = automationTimer;
                  wantedBy = [ "timers.target" ];
                };
              })
            ]
          );
        };

      homeManagerModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.thurbox;
        in
        {
          options.programs.thurbox = options { inherit lib pkgs; };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
            systemd.user = lib.mkIf cfg.automations.enable {
              services.thurbox-automations = {
                Unit.Description = "Fire due thurbox automations (headless)";
                Service = {
                  Type = "oneshot";
                  ExecStart = automationTick cfg.package;
                };
              };
              timers.thurbox-automations = {
                Unit.Description = "Fire due thurbox automations every minute";
                Timer = automationTimer;
                Install.WantedBy = [ "timers.target" ];
              };
            };
          };
        };
    }
    # The systems thurbox releases for. nixpkgs no longer evaluates for
    # x86_64-darwin, and thurbox ships no binary for it either.
    // flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ]
      (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        thurbox = thurboxFor pkgs;

        # Single source of truth for the Rust toolchain: the same
        # rust-toolchain.toml cargo/rustup already honor (stable + rustfmt,
        # clippy, rust-src). No version drift between flake users and others.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Cargo dev tools that ARE packaged in nixpkgs, at the revision flake.lock
        # pins.
        cargoTools = with pkgs; [
          cargo-nextest
          cargo-deny
          cocogitto
        ];

        # System deps to build/test/lint thurbox (mirrors .github/workflows/ci.yml).
        systemTools = with pkgs; [
          tmux # session backend (AGENTS.md: >= 3.2)
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
        # (prek = pre-commit runner, rumdl = markdown linter.)
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
        packages = {
          inherit thurbox;
          default = thurbox;
        };

        # Evaluates the NixOS module with everything on and renders the unit it
        # adds; `nix flake check` alone only checks that the module is a function.
        checks = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          nixos-module =
            let
              nixos = nixpkgs.lib.nixosSystem {
                inherit system;
                modules = [
                  self.nixosModules.default
                  {
                    programs.thurbox = {
                      enable = true;
                      automations.enable = true;
                    };
                    fileSystems."/".device = "none";
                    boot.loader.grub.enable = false;
                    system.stateVersion = "25.11";
                  }
                ];
              };
            in
            pkgs.writeText "thurbox-automations.service"
              nixos.config.systemd.user.units."thurbox-automations.service".text;
        };

        apps.default = {
          type = "app";
          program = "${thurbox}/bin/thurbox";
          meta.description = "Run the thurbox TUI";
        };

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
