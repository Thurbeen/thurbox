{
  lib,
  stdenvNoCC,
  fetchurl,
}:
let
  version = "0.1.22";
  hashes = {
    x86_64-linux = {
      target = "linux-x86_64";
      sha256 = "3054ec3992ce80c5715d721caa9dcefa6056721d15dc113f4750afce763ba5df";
    };
    aarch64-linux = {
      target = "linux-aarch64";
      sha256 = "98757985245d45f8d73dd85e68b139b7f167c28488bb89edd9834c3f5a5cf4cf";
    };
    aarch64-darwin = {
      target = "macos-aarch64";
      sha256 = "90798daadefa8d126b90ef0bcdbe9c7dda9c6cfbf011a247065ab5da348f350e";
    };
  };
  build = hashes.${stdenvNoCC.hostPlatform.system};
in
stdenvNoCC.mkDerivation {
  pname = "herdr-core";
  inherit version;
  src = fetchurl {
    url = "https://releases.herdr.dev/core/${version}/herdr-core-${build.target}.tar.gz";
    inherit (build) sha256;
  };
  sourceRoot = ".";
  installPhase = ''
    install -Dm755 herdr-core $out/bin/herdr-core
  '';
  meta = {
    description = "herdr-core, the terminals runtime-herdr's e2e tests run on";
    homepage = "https://github.com/herdrdev/herdr-core";
    license = lib.licenses.asl20;
    platforms = builtins.attrNames hashes;
    mainProgram = "herdr-core";
  };
}
