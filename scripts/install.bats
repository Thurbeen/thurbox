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

@test "script is under 200 lines" {
  [ $(wc -l < "${BATS_TEST_DIRNAME}/install.sh") -lt 200 ]
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

@test "script supports VERSION env var" {
  grep -q "VERSION" "${BATS_TEST_DIRNAME}/install.sh"
}

@test "script supports INSTALL_DIR env var" {
  grep -q "INSTALL_DIR" "${BATS_TEST_DIRNAME}/install.sh"
}
