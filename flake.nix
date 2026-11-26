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
      # A flake cannot see git tags: the version is Cargo.toml's base plus the commit date.
      baseVersion = nixpkgs.lib.removeSuffix "-dev" (nixpkgs.lib.importTOML ./crates/thurbox/Cargo.toml).package.version;
      date = builtins.substring 0 8 (self.lastModifiedDate or "19700101");
      version = "${baseVersion}-unstable-${builtins.substring 0 4 date}-${builtins.substring 4 2 date}-${builtins.substring 6 2 date}";

      thurboxFor =
        pkgs:
        pkgs.callPackage ./nix/package.nix {
          inherit version;
          rev = self.shortRev or self.dirtyShortRev or null;
        };

      removedAutomations = ''
        thurbox schedules nothing of its own any more. Run work on a schedule
        with a Fleet CronTask, or with a systemd timer that runs
        `fleet send TASK TEXT`.
      '';
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
          imports = [
            (lib.mkRemovedOptionModule [ "programs" "thurbox" "automations" ] removedAutomations)
          ];
          options.programs.thurbox = options { inherit lib pkgs; };
          config = lib.mkIf cfg.enable { environment.systemPackages = [ cfg.package ]; };
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
          imports = [
            (lib.mkRemovedOptionModule [ "programs" "thurbox" "automations" ] removedAutomations)
          ];
          options.programs.thurbox = options { inherit lib pkgs; };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
          };
        };
    }
    # nixpkgs no longer evaluates for x86_64-darwin.
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

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        cargoTools = with pkgs; [
          cargo-nextest
          cargo-deny
          cocogitto
        ];

        systemTools = with pkgs; [
          # The stdenv shell is Bash without interactive builtins such as `bind`.
          bashInteractive
          tmux
          (callPackage ./nix/herdr-core.nix { })
          git
          shellcheck
          selene
          stylua
          lua-language-server
          bats
          nodejs_22
          just
          jq
        ];

        demoTools = with pkgs; [
          vhs
          ffmpeg
          ttyd
        ];

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

        # `nix flake check` alone only checks that the NixOS module is a function.
        checks = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          nixos-module =
            let
              nixos = nixpkgs.lib.nixosSystem {
                inherit system;
                modules = [
                  self.nixosModules.default
                  {
                    programs.thurbox.enable = true;
                    fileSystems."/" = {
                      device = "none";
                      fsType = "tmpfs";
                    };
                    boot.loader.grub.enable = false;
                    system.stateVersion = "25.11";
                  }
                ];
              };
              installed = builtins.elem nixos.config.programs.thurbox.package nixos.config.environment.systemPackages;
            in
            assert pkgs.lib.assertMsg installed "the NixOS module does not install thurbox";
            pkgs.writeText "thurbox-nixos-module" "installed\n";
        };

        apps.default = {
          type = "app";
          program = "${thurbox}/bin/thurbox";
          meta.description = "Run the thurbox TUI";
        };

        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain ] ++ cargoTools ++ systemTools ++ demoTools;

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
