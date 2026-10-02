#!/usr/bin/env bash
set -euo pipefail
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX

root="$(mktemp -d "${TMPDIR:-/tmp}/thurbox-seed-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT
export TBX_SANDBOX_ROOT="$root"
export THURBOX_CONFIG_DIR="$root/thurbox-config"
export THURBOX_DATA_DIR="$root/thurbox-data"
export TMUX_TMPDIR="$root/tmux"
mkdir -p "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR"
cargo build --quiet --bin thurbox-cli

"$(dirname "$0")/seed-sandbox.sh"
"$(dirname "$0")/seed-sandbox.sh"
target/debug/thurbox-cli config validate >/dev/null

python3 - "$root" <<'PY'
import os, pathlib, sqlite3, subprocess, sys

root = pathlib.Path(sys.argv[1])
hosts = (root / "thurbox-config/hosts.toml").read_text()
for name in ("build-box", "offline-box", "slow-box", "lab-wsl", "win-desk"):
    assert f'name = "{name}"' in hosts, name
    assert hosts.count(f'name = "{name}"') == 1, name
repos = root / "repos"
for name in ("web-app", "service-api", "docs-site"):
    repo = repos / name
    assert (repo / ".git").exists(), name
    assert int(subprocess.check_output(["git", "-C", str(repo), "rev-list", "--count", "HEAD"])) >= 2
    assert subprocess.check_output(["git", "-C", str(repo), "branch", "--list", "feature/demo"]).strip()
    assert subprocess.check_output(["git", "-C", str(repo), "branch", "-r"]).strip()
    assert subprocess.check_output(["git", "-C", str(repo), "remote", "get-url", "origin"]).strip() == str(root / "remotes" / f"{name}.git").encode()
    assert subprocess.check_output(["git", "-C", str(repo), "status", "--porcelain"]).strip()
    assert (repo / "README.md").read_text().count("Uncommitted example.") == 1
db = sqlite3.connect(root / "thurbox-data/thurbox.db")
paths = {row[0] for row in db.execute("SELECT repo_path FROM repo_bookmarks WHERE host = ''")}
assert paths == {str(repos / name) for name in ("web-app", "service-api", "docs-site")}, paths
if (root / "mock-bin/ssh").exists():
    env = dict(os.environ, TBX_SANDBOX_ROOT=str(root))
    ssh = str(root / "mock-bin/ssh")
    assert subprocess.check_output([ssh, "build-box", "printf reachable"], env=env) == b"reachable"
    assert subprocess.check_output([ssh, "build-box", "printf $TMUX_TMPDIR"], env=env) == str(root / "tmux/build-box").encode()
    assert subprocess.run([ssh, "offline-box", "true"], env=env).returncode == 255
    assert subprocess.check_output([ssh, "slow-box", "printf delayed"], env=env) == b"delayed"
    assert subprocess.check_output([ssh, "slow-box", "printf $TMUX_TMPDIR"], env=env) == str(root / "tmux/slow-box").encode()
    wsl = str(root / "mock-bin/wsl.exe")
    assert subprocess.check_output([wsl, "-l", "-q"], env=env) == b"lab-wsl\n"
    assert subprocess.check_output([wsl, "-d", "lab-wsl", "--", "printf", "wsl"], env=env) == b"wsl"
    assert subprocess.check_output([wsl, "-d", "lab-wsl", "--", "sh", "-c", "printf $TMUX_TMPDIR"], env=env) == str(root / "tmux/lab-wsl").encode()
print("sandbox seed: hosts, repositories, and bookmarks verified")
PY
