#!/usr/bin/env bats

@test "script has valid shell syntax" {
  sh -n "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script is executable" {
  [ -x "${BATS_TEST_DIRNAME}/install.sh" ]
}

@test "script has POSIX shebang" {
  head -1 "${BATS_TEST_DIRNAME}/install.sh" | grep -q "#!/usr/bin/env sh"
}

@test "script contains cleanup function" {
  grep -q "^cleanup()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains detect_platform function" {
  grep -q "^detect_platform()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains get_target function" {
  grep -q "^get_target()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains cmd_exists function" {
  grep -q "^cmd_exists()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains download function" {
  grep -q "^download()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains get_version function" {
  grep -q "^get_version()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains get_checksum function" {
  grep -q "^get_checksum()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains check_sum function" {
  grep -q "^check_sum()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains get_binary function" {
  grep -q "^get_binary()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains do_install function" {
  grep -q "^do_install()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains show_success function" {
  grep -q "^show_success()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script contains main function" {
  grep -q "^main()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script has trap cleanup" {
  grep -q "trap cleanup" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script is under 290 lines" {
  [ "$(wc -l < "${BATS_TEST_DIRNAME}/install.sh")" -lt 290 ]
}

@test "script contains banner function" {
  grep -q "^banner()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "banner renders ASCII art to stderr" {
  run sh -c "TEST_TMPDIR=1 . '${BATS_TEST_DIRNAME}/install.sh'; banner" 2>&1
  [ "$status" -eq 0 ]
  [ -n "$output" ]
}

@test "colors disabled when NO_COLOR is set" {
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 . '${BATS_TEST_DIRNAME}/install.sh'; printf '%s' \"\$C_RED\""
  [ -z "$output" ]
}

@test "script has logging functions" {
  grep -q "^info()" "${BATS_TEST_DIRNAME}/install.sh"
  grep -q "^error()" "${BATS_TEST_DIRNAME}/install.sh"
  grep -q "^success()" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script maps linux-x86_64 to musl" {
  grep -q "linux-x86_64) echo \"x86_64-unknown-linux-musl\"" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script maps darwin-aarch64 correctly" {
  grep -q "darwin-aarch64) echo \"aarch64-apple-darwin\"" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "get_target maps linux-aarch64 and linux-arm64 to musl" {
  for platform in linux-aarch64 linux-arm64; do
    run sh -c "TEST_TMPDIR=1 . '${BATS_TEST_DIRNAME}/install.sh'; get_target $platform"
    [ "$status" -eq 0 ]
    [ "$output" = "aarch64-unknown-linux-musl" ]
  done
}

@test "get_target rejects unshipped platforms (darwin-x86_64)" {
  # cd.yml builds no .tar.gz for it, so get_target must NOT resolve it.
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 . '${BATS_TEST_DIRNAME}/install.sh'; get_target darwin-x86_64"
  [ "$status" -ne 0 ]
}

@test "script supports VERSION env var" {
  grep -q "VERSION" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script supports INSTALL_DIR env var" {
  grep -q "INSTALL_DIR" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "do_install succeeds with only the thurbox binary in tarball" {
  tmpdir=$(mktemp -d)
  mkdir -p "$tmpdir/src" "$tmpdir/dest"
  printf '#!/bin/sh\n' > "$tmpdir/src/thurbox"
  tar -czf "$tmpdir/archive.tar.gz" -C "$tmpdir/src" thurbox
  TEST_TMPDIR=1 sh -c ". '${BATS_TEST_DIRNAME}/install.sh'; do_install '$tmpdir/archive.tar.gz' '$tmpdir/dest'"
  [ -x "$tmpdir/dest/thurbox" ]
  rm -rf "$tmpdir"
}

@test "do_install chmods both binaries when thurbox-cli is present" {
  tmpdir=$(mktemp -d)
  mkdir -p "$tmpdir/src" "$tmpdir/dest"
  printf '#!/bin/sh\n' > "$tmpdir/src/thurbox"
  printf '#!/bin/sh\n' > "$tmpdir/src/thurbox-cli"
  tar -czf "$tmpdir/archive.tar.gz" -C "$tmpdir/src" thurbox thurbox-cli
  TEST_TMPDIR=1 sh -c ". '${BATS_TEST_DIRNAME}/install.sh'; do_install '$tmpdir/archive.tar.gz' '$tmpdir/dest'"
  [ -x "$tmpdir/dest/thurbox" ]
  [ -x "$tmpdir/dest/thurbox-cli" ]
  rm -rf "$tmpdir"
}


@test "do_install makes every shipped program executable" {
  tmpdir=$(mktemp -d)
  mkdir -p "$tmpdir/src" "$tmpdir/dest"
  shipped=$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries)
  for bin in $shipped; do
    printf '#!/bin/sh\n' > "$tmpdir/src/$bin"
    chmod -x "$tmpdir/src/$bin"
  done
  tar -czf "$tmpdir/archive.tar.gz" -C "$tmpdir/src" .
  TEST_TMPDIR=1 sh -c ". '${BATS_TEST_DIRNAME}/install.sh'; do_install '$tmpdir/archive.tar.gz' '$tmpdir/dest'"
  for bin in $shipped; do
    [ -x "$tmpdir/dest/$bin" ]
  done
  rm -rf "$tmpdir"
}

@test "installs the programs the release archive ships" {
  expected=$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries | tr '\n' ' ' | sed 's/ $//')
  run sh -c "TEST_TMPDIR=1 . '${BATS_TEST_DIRNAME}/install.sh'; printf '%s' \"\$BINARIES\""
  [ "$output" = "$expected" ]
}

# A 3.x release names its sums file thurbox-<V>-sha256sums.txt, never
# thurbox-v<V>-checksums.txt: that name is what every 2.x updater fetches
# first (docs/thurbox/releasing.md, "What a 3.x release must do").
@test "get_binary downloads from DOWNLOAD_URL, verified by the 3.x sums file" {
  tmpdir=$(mktemp -d)
  mkdir -p "$tmpdir/v9.9.9"
  printf 'payload' > "$tmpdir/v9.9.9/thurbox-v9.9.9-x86_64-unknown-linux-musl.tar.gz"
  (cd "$tmpdir/v9.9.9" && sha256sum thurbox-v9.9.9-x86_64-unknown-linux-musl.tar.gz \
    > thurbox-9.9.9-sha256sums.txt)
  mkdir "$tmpdir/work"
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 DOWNLOAD_URL='file://$tmpdir' . '${BATS_TEST_DIRNAME}/install.sh'; get_binary v9.9.9 x86_64-unknown-linux-musl '$tmpdir/work'"
  [ "$status" -eq 0 ]
  [ "$(cat "$tmpdir/work/binary.tar.gz")" = "payload" ]
  rm -rf "$tmpdir"
}

# Rule 1 of docs/thurbox/releasing.md, "What a 3.x release must do": every
# 2.x updater fetches thurbox-v<V>-checksums.txt first, so a 3.x release that
# never publishes that name makes them all fail safe.
@test "a 3.x release's assets carry no thurbox-v<V>-checksums.txt" {
  dir=$(mktemp -d)
  for t in x86_64-unknown-linux-gnu aarch64-apple-darwin; do
    printf '%s' "$t" > "$dir/thurbox-v3.0.0-$t.tar.gz"
  done
  printf 'zip' > "$dir/thurbox-v3.0.0-x86_64-pc-windows-msvc.zip"
  run "${BATS_TEST_DIRNAME}/ci/release-archive.sh" --sums v3.0.0 "$dir"
  [ "$status" -eq 0 ]
  [ "$output" = "$dir/thurbox-3.0.0-sha256sums.txt" ]
  [ -z "$(find "$dir" -name 'thurbox-v*-checksums.txt')" ]
  [ "$(wc -l < "$dir/thurbox-3.0.0-sha256sums.txt")" -eq 3 ]
  (cd "$dir" && sha256sum -c thurbox-3.0.0-sha256sums.txt)
  rm -rf "$dir"
}

@test "the Release workflow publishes the sums file under no other name" {
  cd_yml="${BATS_TEST_DIRNAME}/../.github/workflows/cd.yml"
  grep -q 'release-archive.sh --sums' "$cd_yml"
  # Comments may name the old file to say why it is gone; no step may.
  run sh -c "grep -vE '^[[:space:]]*#' '$cd_yml' | grep -nE 'thurbox-[^ \"/]*-checksums\\.txt|\\*checksums\\.txt'"
  [ "$status" -eq 1 ]
}

# VERSION=v2.x.y pins a 2.x release, which only has the old sums name.
@test "get_binary verifies a 2.x release by its checksums.txt" {
  tmpdir=$(mktemp -d)
  mkdir -p "$tmpdir/v2.84.0"
  printf 'payload' > "$tmpdir/v2.84.0/thurbox-v2.84.0-x86_64-unknown-linux-musl.tar.gz"
  (cd "$tmpdir/v2.84.0" && sha256sum thurbox-v2.84.0-x86_64-unknown-linux-musl.tar.gz \
    > thurbox-v2.84.0-checksums.txt)
  mkdir "$tmpdir/work"
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 DOWNLOAD_URL='file://$tmpdir' . '${BATS_TEST_DIRNAME}/install.sh'; get_binary v2.84.0 x86_64-unknown-linux-musl '$tmpdir/work'"
  [ "$status" -eq 0 ]
  [ "$(cat "$tmpdir/work/binary.tar.gz")" = "payload" ]
  rm -rf "$tmpdir"
}

@test "show_success points at fleet init only when fleet was installed" {
  tmpdir=$(mktemp -d)
  touch "$tmpdir/thurbox" "$tmpdir/thurbox-cli"
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 . '${BATS_TEST_DIRNAME}/install.sh'; show_success '$tmpdir' 2>&1"
  [ "$status" -eq 0 ]
  [[ "$output" != *"fleet init"* ]]
  [[ "$output" == *"no Fleet"* ]]
  touch "$tmpdir/fleet"
  run sh -c "TEST_TMPDIR=1 NO_COLOR=1 . '${BATS_TEST_DIRNAME}/install.sh'; show_success '$tmpdir' 2>&1"
  [[ "$output" == *"fleet init"* ]]
  [[ "$output" != *"no Fleet"* ]]
  rm -rf "$tmpdir"
}

@test "Fleet's programs are some of the shipped ones" {
  shipped=" $("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries | tr '\n' ' ')"
  fleet=$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --fleet-binaries)
  [ -n "$fleet" ]
  for bin in $fleet; do [[ "$shipped" == *" $bin "* ]]; done
  [[ "$fleet" != *thurbox* ]]
}

@test "a Fleet install ships runtime-herdr beside runtime-mux" {
  fleet=" $("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --fleet-binaries | tr '\n' ' ')"
  [[ "$fleet" == *" runtime-mux "* ]]
  [[ "$fleet" == *" runtime-herdr "* ]]
}

@test "installs the Fleet programs the release archive names" {
  expected=$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --fleet-binaries | tr '\n' ' ' | sed 's/ $//')
  run sh -c "TEST_TMPDIR=1 . '${BATS_TEST_DIRNAME}/install.sh'; printf '%s' \"\$FLEET_BINARIES\""
  [ "$output" = "$expected" ]
}

# A release of every shipped program for this machine, served from a directory
# as DOWNLOAD_URL fetches it. Sets $assets and $dest.
fake_release() {
  tmpdir=$(mktemp -d)
  assets="$tmpdir/www" dest="$tmpdir/bin"
  mkdir -p "$tmpdir/src" "$assets/v9.9.9"
  files="$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries) LICENSE"
  for f in $files; do printf '#!/bin/sh\n' > "$tmpdir/src/$f"; done
  printf '#!/bin/sh\necho "$*" >> "%s/fleet-calls"\n' "$tmpdir" > "$tmpdir/src/fleet"
  target=$(TEST_TMPDIR=1 sh -c ". '${BATS_TEST_DIRNAME}/install.sh'; get_target \$(detect_platform)")
  # Named members, as release-archive.sh packs them.
  # shellcheck disable=SC2086 # one member per word
  tar -czf "$assets/v9.9.9/thurbox-v9.9.9-$target.tar.gz" -C "$tmpdir/src" $files
  (cd "$assets/v9.9.9" && sha256sum ./*.tar.gz | sed 's| \./| |' > thurbox-9.9.9-sha256sums.txt)
}

# Sets $code and $log (stdout and stderr).
run_installer() {
  code=0
  log=$(env NO_COLOR=1 THURBOX_NO_SERVICE=1 VERSION=v9.9.9 DOWNLOAD_URL="file://$assets" INSTALL_DIR="$dest" "$@" 2>&1) || code=$?
}

installed() { find "$dest" -mindepth 1 -maxdepth 1 -exec basename {} \; | sort | tr '\n' ' ' | sed 's/ $//'; }

fleet_only_installed() {
  [ "$code" -eq 0 ]
  [ "$(installed)" = "$("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --fleet-binaries | sort | tr '\n' ' ' | sed 's/ $//')" ]
  for step in "fleet init" "fleet up" "fleet apply" "fleet doctor"; do
    [[ "$log" == *"$step"* ]]
  done
  [[ "$log" != *"thurbox-cli"* ]]
  [[ "$log" != *"Launch the TUI"* ]]
}

@test "--fleet-only installs only Fleet's programs and names only Fleet's steps" {
  fake_release
  run_installer sh "${BATS_TEST_DIRNAME}/install.sh" --fleet-only
  fleet_only_installed
  rm -rf "$tmpdir"
}

@test "THURBOX_FLEET_ONLY=1 is the same as --fleet-only, and says it skipped thurbox" {
  fake_release
  run_installer THURBOX_FLEET_ONLY=1 sh "${BATS_TEST_DIRNAME}/install.sh"
  fleet_only_installed
  [[ "$log" == *"THURBOX_FLEET_ONLY is set"* ]]
  rm -rf "$tmpdir"
}

@test "--fleet-only does not blame THURBOX_FLEET_ONLY" {
  fake_release
  run_installer THURBOX_FLEET_ONLY= sh "${BATS_TEST_DIRNAME}/install.sh" --fleet-only
  fleet_only_installed
  [[ "$log" != *"THURBOX_FLEET_ONLY"* ]]
  rm -rf "$tmpdir"
}

# A 2.x release, as VERSION=v2.x.y pins it: two programs, and only the old
# sums name. Sets $assets and $dest, as fake_release does.
fake_v2_release() {
  tmpdir=$(mktemp -d)
  assets="$tmpdir/www" dest="$tmpdir/bin"
  mkdir -p "$tmpdir/src" "$assets/v2.89.0"
  for f in thurbox thurbox-cli LICENSE; do printf '#!/bin/sh\n' > "$tmpdir/src/$f"; done
  target=$(TEST_TMPDIR=1 sh -c ". '${BATS_TEST_DIRNAME}/install.sh'; get_target \$(detect_platform)")
  tar -czf "$assets/v2.89.0/thurbox-v2.89.0-$target.tar.gz" -C "$tmpdir/src" thurbox thurbox-cli LICENSE
  (cd "$assets/v2.89.0" && sha256sum ./*.tar.gz | sed 's| \./| |' > thurbox-v2.89.0-checksums.txt)
}

@test "a pinned 2.x release installs, verified by its checksums.txt" {
  fake_v2_release
  run_installer VERSION=v2.89.0 sh "${BATS_TEST_DIRNAME}/install.sh"
  [ "$code" -eq 0 ]
  [ -x "$dest/thurbox" ]
  [ -x "$dest/thurbox-cli" ]
  [ ! -e "$dest/fleet" ]
  rm -rf "$tmpdir"
}

@test "THURBOX_FLEET_ONLY=0 is a full install" {
  fake_release
  run_installer THURBOX_FLEET_ONLY=0 sh "${BATS_TEST_DIRNAME}/install.sh"
  [ "$code" -eq 0 ]
  for bin in $("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries); do
    [ -x "$dest/$bin" ]
  done
  rm -rf "$tmpdir"
}

@test "a full install still installs every shipped program and names thurbox" {
  fake_release
  run_installer THURBOX_FLEET_ONLY= sh "${BATS_TEST_DIRNAME}/install.sh"
  [ "$code" -eq 0 ]
  for bin in $("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries); do
    [ -x "$dest/$bin" ]
  done
  [[ "$log" == *"thurbox-cli doctor"* ]]
  rm -rf "$tmpdir"
}

@test "an unknown option fails before anything is installed" {
  fake_release
  run_installer sh "${BATS_TEST_DIRNAME}/install.sh" --fleet-onyl
  [ "$code" -ne 0 ]
  [[ "$log" == *"--fleet-onyl"* ]]
  [ ! -e "$dest" ]
  rm -rf "$tmpdir"
}

fake_systemctl() {
  mkdir -p "$tmpdir/systemd-bin"
  printf '#!/bin/sh\nexit 0\n' > "$tmpdir/systemd-bin/systemctl"
  chmod +x "$tmpdir/systemd-bin/systemctl"
  path="$tmpdir/systemd-bin:$PATH"
}

@test "on Linux with a user systemd, an install has Fleet install and start its service" {
  [ "$(uname -s)" = Linux ] || skip "the service backend is systemd"
  for flag in "" --fleet-only; do
    fake_release
    fake_systemctl
    # shellcheck disable=SC2086 # the flag is a word or none
    run_installer THURBOX_NO_SERVICE= PATH="$path" sh "${BATS_TEST_DIRNAME}/install.sh" $flag
    [ "$code" -eq 0 ]
    [ "$(cat "$tmpdir/fleet-calls")" = "$(printf 'init\nservice install --now')" ]
    [[ "$log" == *"fleet service status"* ]]
    [[ "$log" != *"fleet up"* ]]
    rm -rf "$tmpdir"
  done
}

@test "THURBOX_NO_SERVICE=1 installs no service" {
  fake_release
  fake_systemctl
  run_installer PATH="$path" sh "${BATS_TEST_DIRNAME}/install.sh" --fleet-only
  fleet_only_installed
  [ ! -e "$tmpdir/fleet-calls" ]
  rm -rf "$tmpdir"
}

@test "an install puts the release under lib/thurbox and links bin to current" {
  fake_release
  printf 'old' > "$tmpdir/stale"
  mkdir -p "$dest" && cp "$tmpdir/stale" "$dest/thurbox"
  run_installer sh "${BATS_TEST_DIRNAME}/install.sh"
  [ "$code" -eq 0 ]
  home="$tmpdir/lib/thurbox"
  [ "$(readlink "$home/current")" = releases/9.9.9 ]
  [ "$(readlink "$home/install-dir")" = "$(cd "$dest" && pwd)" ]
  [ ! -e "$home/previous" ]
  for bin in $("${BATS_TEST_DIRNAME}/ci/release-archive.sh" --binaries); do
    [ -x "$home/releases/9.9.9/bin/$bin" ]
    [ "$(readlink "$dest/$bin")" = "$home/current/bin/$bin" ]
  done
  rm -rf "$tmpdir"
}

@test "a second install keeps the first release as previous" {
  fake_release
  run_installer sh "${BATS_TEST_DIRNAME}/install.sh"
  mv "$assets/v9.9.9" "$assets/v9.9.10"
  for f in "$assets"/v9.9.10/*; do mv "$f" "$(echo "$f" | sed 's/9\.9\.9/9.9.10/')"; done
  (cd "$assets/v9.9.10" && sha256sum ./*.tar.gz | sed 's| \./| |' > thurbox-9.9.10-sha256sums.txt)
  run_installer VERSION=v9.9.10 sh "${BATS_TEST_DIRNAME}/install.sh"
  [ "$code" -eq 0 ]
  [ "$(readlink "$tmpdir/lib/thurbox/current")" = releases/9.9.10 ]
  [ "$(readlink "$tmpdir/lib/thurbox/previous")" = releases/9.9.9 ]
  rm -rf "$tmpdir"
}

@test "a Fleet-only install over a full one keeps thurbox" {
  fake_release
  run_installer sh "${BATS_TEST_DIRNAME}/install.sh"
  mv "$assets/v9.9.9" "$assets/v9.9.10"
  for f in "$assets"/v9.9.10/*; do mv "$f" "$(echo "$f" | sed 's/9\.9\.9/9.9.10/')"; done
  (cd "$assets/v9.9.10" && sha256sum ./*.tar.gz | sed 's| \./| |' > thurbox-9.9.10-sha256sums.txt)
  run_installer VERSION=v9.9.10 sh "${BATS_TEST_DIRNAME}/install.sh" --fleet-only
  [ "$code" -eq 0 ]
  [ -x "$dest/thurbox" ]
  [ -x "$tmpdir/lib/thurbox/releases/9.9.10/bin/thurbox" ]
  rm -rf "$tmpdir"
}
