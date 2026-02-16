# Test helpers for install.sh testing
# Sourced by bats tests to provide mocking utilities

# Mock commands
mock_uname() {
  local arg="$1"
  case "$arg" in
    -s) echo "$MOCK_UNAME_S" ;;
    -m) echo "$MOCK_UNAME_M" ;;
    *) uname "$arg" ;;
  esac
}

mock_curl() {
  # Check if this is a GitHub API call (fetch latest version)
  if [[ "$1" == *"api.github.com"* ]]; then
    if [ -n "$MOCK_CURL_API_RESPONSE" ]; then
      echo "$MOCK_CURL_API_RESPONSE"
      return 0
    fi
    return 1
  fi
  # For other curl calls, use real curl
  command curl "$@"
}

mock_command_not_found() {
  # Return 1 (command not found) for specific commands
  if [ "$1" = "curl" ] && [ "$MOCK_NO_CURL" = "1" ]; then
    return 1
  fi
  if [ "$1" = "wget" ] && [ "$MOCK_NO_WGET" = "1" ]; then
    return 1
  fi
  command -v "$1" > /dev/null 2>&1
}

# Setup test environment
setup_test_dir() {
  TEST_TMPDIR=$(mktemp -d)
  export TEST_TMPDIR
  cd "$TEST_TMPDIR"
}

# Cleanup test environment
cleanup_test_dir() {
  cd /
  rm -rf "$TEST_TMPDIR"
}

# Create a mock tarball with binary
create_mock_tarball() {
  local output_file="$1"
  local tarball_dir=$(mktemp -d)

  # Create a fake binary
  echo "#!/bin/sh" > "$tarball_dir/thurbox"
  echo "echo 'v0.1.0'" >> "$tarball_dir/thurbox"
  chmod +x "$tarball_dir/thurbox"

  # Create LICENSE file
  echo "MIT License" > "$tarball_dir/LICENSE"

  # Create tarball
  tar -czf "$output_file" -C "$tarball_dir" thurbox LICENSE

  rm -rf "$tarball_dir"
}

# Create a mock checksums file
create_mock_checksums() {
  local checksums_file="$1"
  local binary_file="$2"
  local target="$3"

  local sha256=$(sha256sum "$binary_file" | awk '{print $1}')
  echo "${sha256}  thurbox-v0.1.0-${target}.tar.gz" > "$checksums_file"
}

# Export mocking functions so they're available in subshells
export -f mock_uname
export -f mock_curl
export -f mock_command_not_found
export -f setup_test_dir
export -f cleanup_test_dir
export -f create_mock_tarball
export -f create_mock_checksums
