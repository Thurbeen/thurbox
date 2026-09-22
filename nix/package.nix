# thurbox as a Nix package. Kept out of flake.nix so the same file can be
# `callPackage`d from an overlay, the flake's own `packages`, or a nixpkgs
# checkout.
{
  lib,
  rustPlatform,
  makeWrapper,
  git,
  tmux,
  # Set by the flake from its own source; a caller without one gets a
  # version that still reads as unreleased.
  version ? "0.0.0-unstable",
  rev ? null,
}:

rustPlatform.buildRustPackage {
  pname = "thurbox";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../src
      ../ui
      ../extensions
      ../examples
      # Named by Cargo.toml's [[bench]], so the manifest does not load without it.
      ../benches
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  # build.rs takes the version from here. It must not contain `-dev`: that
  # turns on the `dev_build` cfg, which moves a build onto the `thurbox-dev`
  # tmux socket and data directory. A `0.0.0` version still counts as
  # unreleased at runtime (`is_dev_version`), which is what keeps
  # `thurbox-cli update` from trying to replace a binary in the read-only store.
  env.THURBOX_RELEASE_VERSION = version + lib.optionalString (rev != null) "+${rev}";

  cargoBuildFlags = [
    "--bin"
    "thurbox"
    "--bin"
    "thurbox-cli"
  ];

  # The library's and binaries' unit tests, under nextest like CI: they share
  # process-global state (a host's learned socket, the host-CLI fakes), so
  # under plain `cargo test` one test's leftovers fail another. The integration
  # tests under tests/ drive a real terminal, which the build sandbox does not
  # provide; CI runs them.
  useNextest = true;
  cargoTestFlags = [
    "--lib"
    "--bins"
  ];
  checkType = "debug";
  nativeCheckInputs = [
    git
    tmux
  ];

  nativeBuildInputs = [ makeWrapper ];

  # tmux is the session backend and git drives worktrees. Appended rather than
  # prepended, so a tmux or git the user already has keeps winning, and so the
  # agents thurbox starts see the same PATH they would without Nix.
  postInstall = ''
    for bin in thurbox thurbox-cli; do
      wrapProgram "$out/bin/$bin" --suffix PATH : ${
        lib.makeBinPath [
          tmux
          git
        ]
      }
    done
  '';

  meta = {
    description = "TUI for orchestrating multiple coding-agent CLI sessions in persistent tmux panels";
    homepage = "https://github.com/Thurbeen/thurbox";
    license = lib.licenses.mit;
    mainProgram = "thurbox";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
