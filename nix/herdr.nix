{
  lib,
  stdenvNoCC,
  fetchurl,
}:
let
  version = "0.9.5";
  assets = {
    x86_64-linux = {
      name = "herdr-linux-x86_64";
      sha256 = "14d9e8bc8b4a9e0118d77c32266f76d461f09b40342eb23752960bd89f59929d";
    };
    aarch64-linux = {
      name = "herdr-linux-aarch64";
      sha256 = "17d7536f2da50859523680ef279dffcac8e4a235f4e46fe8da6c2ec44141897c";
    };
    aarch64-darwin = {
      name = "herdr-macos-aarch64";
      sha256 = "fb77c2232a05a8035421382c894392f4174e4e38a18e48af3dd0290157c3a7e2";
    };
  };
  asset = assets.${stdenvNoCC.hostPlatform.system};
in
stdenvNoCC.mkDerivation {
  pname = "herdr";
  inherit version;
  src = fetchurl {
    url = "https://github.com/herdrdev/herdr/releases/download/v${version}/${asset.name}";
    inherit (asset) sha256;
  };
  dontUnpack = true;
  installPhase = ''
    install -Dm755 $src $out/bin/herdr
  '';
  meta = {
    description = "Terminal multiplexer for coding agents, the runtime-herdr e2e tests' herdr";
    homepage = "https://github.com/herdrdev/herdr";
    license = lib.licenses.asl20;
    platforms = builtins.attrNames assets;
    mainProgram = "herdr";
  };
}
