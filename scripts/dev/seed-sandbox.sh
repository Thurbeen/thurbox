#!/usr/bin/env bash
# Seed only an initialized sandbox. The caller supplies its isolated paths.
set -euo pipefail

# Git exports these into hooks; a seed invoked from a hook must still write
# solely to its throwaway repositories.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

: "${TBX_SANDBOX_ROOT:?initialize a sandbox first}"
: "${THURBOX_CONFIG_DIR:?initialize a sandbox first}"
: "${THURBOX_DATA_DIR:?initialize a sandbox first}"
: "${TMUX_TMPDIR:?initialize a sandbox first}"
case "$THURBOX_CONFIG_DIR:$THURBOX_DATA_DIR" in
    "$TBX_SANDBOX_ROOT"/*:"$TBX_SANDBOX_ROOT"/*) ;;
    *) echo 'seed-sandbox: config and data must be inside the sandbox' >&2; exit 2 ;;
esac

mkdir -p "$THURBOX_CONFIG_DIR" "$THURBOX_DATA_DIR" \
    "$TBX_SANDBOX_ROOT/repos" "$TBX_SANDBOX_ROOT/remotes"
hosts="$THURBOX_CONFIG_DIR/hosts.toml"
if [ ! -e "$hosts" ]; then
    : > "$hosts"
fi
if ! grep -q '^# sandbox mock hosts$' "$hosts"; then
    cat >> "$hosts" <<'TOML'

# sandbox mock hosts
[[hosts]]
name = "build-box"
destination = "build-box"
share_sessions = false

[[hosts]]
name = "offline-box"
destination = "offline-box"
share_sessions = false

[[hosts]]
name = "slow-box"
destination = "slow-box"
share_sessions = false

[[hosts]]
name = "lab-wsl"
kind = "wsl"
distro = "lab-wsl"
share_sessions = false

[[hosts]]
name = "win-desk"
destination = "win-desk"
platform = "windows"
multiplexer = "psmux"
share_sessions = false
TOML
fi

for name in web-app service-api docs-site; do
    repo="$TBX_SANDBOX_ROOT/repos/$name"
    if [ ! -d "$repo/.git" ]; then
        mkdir -p "$repo"
        git -C "$repo" init -q -b main
        git -C "$repo" config user.name 'Sandbox Example'
        git -C "$repo" config user.email 'sandbox@example.invalid'
        printf '# %s\n' "$name" > "$repo/README.md"
        git -C "$repo" add README.md
        git -C "$repo" commit -qm 'Initial example'
        printf 'A second revision.\n' >> "$repo/README.md"
        git -C "$repo" add README.md
        git -C "$repo" commit -qm 'Add example content'
        git -C "$repo" branch feature/demo
        remote="$TBX_SANDBOX_ROOT/remotes/$name.git"
        git init -q --bare "$remote"
        git -C "$repo" remote add origin "$remote"
        git -C "$repo" push -q -u origin main
        printf 'Uncommitted example.\n' >> "$repo/README.md"
    fi
done

# The POSIX test relay deliberately accepts only our fixture aliases. It
# mirrors the local ssh stand-in in tests/backend_routes.rs.
case "$(uname -s)" in
    Linux*)
        mkdir -p "$TBX_SANDBOX_ROOT/mock-bin" "$TBX_SANDBOX_ROOT/mock-home"
        cat > "$TBX_SANDBOX_ROOT/mock-bin/ssh" <<'SH'
#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o|-p|-i) shift 2 ;;
        -*) shift ;;
        *) break ;;
    esac
done
dest="${1:-}"
[ "$#" -gt 0 ] && shift
case "$dest" in
    build-box|slow-box)
        [ "$dest" = slow-box ] && sleep 2
        export HOME="$TBX_SANDBOX_ROOT/mock-home"
        TMUX_TMPDIR="$TMUX_TMPDIR/$dest"
        export TMUX_TMPDIR
        mkdir -p "$TMUX_TMPDIR"
        exec sh -c "$*" ;;
    offline-box|win-desk) exit 255 ;;
    *) echo "unknown sandbox host: $dest" >&2; exit 255 ;;
esac
SH
        cat > "$TBX_SANDBOX_ROOT/mock-bin/wsl.exe" <<'SH'
#!/bin/sh
case "${1:-}" in
    -l) printf 'lab-wsl\n'; exit 0 ;;
    -d) [ "${2:-}" = lab-wsl ] || exit 1; shift 2 ;;
    *) exit 1 ;;
esac
[ "${1:-}" = -- ] && shift
export HOME="$TBX_SANDBOX_ROOT/mock-home"
TMUX_TMPDIR="$TMUX_TMPDIR/lab-wsl"
export TMUX_TMPDIR
mkdir -p "$TMUX_TMPDIR"
exec "$@"
SH
        chmod +x "$TBX_SANDBOX_ROOT/mock-bin/ssh" "$TBX_SANDBOX_ROOT/mock-bin/wsl.exe"
        PATH="$TBX_SANDBOX_ROOT/mock-bin:$PATH"
        export PATH
        ;;
esac

# The CLI creates/migrates the schema. Its repo picker keeps bookmarks in the
# same database, with an empty host key for local repositories.
"${TBX_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}/target/debug/thurbox-cli" session list --json >/dev/null
python3 - "$THURBOX_DATA_DIR/thurbox.db" "$TBX_SANDBOX_ROOT/repos" <<'PY'
import pathlib, sqlite3, sys, time

db = sqlite3.connect(sys.argv[1])
repos = pathlib.Path(sys.argv[2])
for name in ('web-app', 'service-api', 'docs-site'):
    db.execute('''INSERT OR IGNORE INTO repo_bookmarks
        (host, repo_path, last_used_at, use_count, is_parent, is_git)
        VALUES ('', ?, ?, 1, 0, 1)''',
        (str(repos / name), int(time.time() * 1000)))
db.commit()
PY
