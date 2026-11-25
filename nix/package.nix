{
  lib,
  rustPlatform,
  makeWrapper,
  git,
  tmux,
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
      ../crates
      ../ui
      ../examples
      ../vendor
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  env.THURBOX_RELEASE_VERSION = version + lib.optionalString (rev != null) "+${rev}";

  cargoBuildFlags = [
    "--bin"
    "thurbox"
    "--bin"
    "thurbox-cli"
  ];

  doCheck = false;

  nativeBuildInputs = [ makeWrapper ];

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
