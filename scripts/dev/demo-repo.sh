#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Seed a sandbox with a repository that has something to look at.

`just sandbox` gives you a working thurbox with NOTHING in it — no repos, no
sessions, no changes. Every feature that is about a repository (the review
pane, the file viewer, sync, fork, the creation flow's branch list) is
therefore untestable there until you hand-build a git repo, create a session,
and change files in its worktree. This does that, once, reproducibly.

A session is a Fleet Task (ADR-41) over a worktree this script makes. A Fleet
session row carries no base branch, so the review shows the worktree's
UNCOMMITTED changes (`git diff HEAD`): the fixtures are left uncommitted.

Usage:
  scripts/dev/demo-repo.sh                  # seed the "default" profile
  scripts/dev/demo-repo.sh --profile foo
  scripts/dev/demo-repo.sh --fresh          # seed a throwaway sandbox
  scripts/dev/demo-repo.sh --big            # add the large-diff repo too
  scripts/dev/sandbox.sh --demo             # seed, then launch the TUI

Idempotent PER THING, not per run: the repository and each session are checked
separately, so a session you deleted is recreated while the repository it came
from is left alone.

It never touches your real config: everything lands under the sandbox root
(`repos/` beside `thurbox-config/`), so `--clean` takes it with the profile and
`--fresh` gets a new one. Deliberately NOT under the config dir, where the
interface's own inventory would find it.
EOF
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TBX_REPO_ROOT="${TBX_REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
export TBX_REPO_ROOT

log() { printf '\033[1;36m==>\033[0m %s\n' "$*" >&2; }
skip() { printf '\033[1;30m  ·\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

mode="persistent"
profile="default"
want_big=0
while [ $# -gt 0 ]; do
    case "$1" in
        --fresh) mode="fresh"; shift ;;
        --profile) profile="${2:?--profile needs a name}"; shift 2 ;;
        --big) want_big=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

root_of_calling_sandbox="${TBX_SANDBOX_ROOT:-}"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib/sandbox-env.sh"
if [ -n "$root_of_calling_sandbox" ]; then
    TBX_SANDBOX_ROOT="$root_of_calling_sandbox"
else
    tbx_sandbox_init "$mode" "$profile"
fi
tbx_sandbox_fleet
command -v fleet >/dev/null \
    || die "no fleet on PATH — \`cargo build --bin fleet --bin runtime-mux\` first"

REPOS="$TBX_SANDBOX_ROOT/repos"
mkdir -p "$REPOS"

UNPROFILED_HARNESS_FLEET_RUNS_AS_A_PLAIN_SHELL="sh"

seed_lua_whose_deleted_line_diffs_as_triple_dash() {
    cat > "$1/src/one.lua" <<'EOF'
-- one.lua
local function greet(name)
  return "hello " .. name
end
return greet
EOF
}

seed_nested_file_that_git_interleaves_between_siblings() {
    cat > "$1/src/deep/nested/two.rs" <<'EOF'
fn main() {
    println!("one");
}
EOF
}

seed_file_to_rename_with_an_edit() {
    cat > "$1/docs/notes.md" <<'EOF'
# Notes
first
EOF
}

seed_file_to_delete() {
    printf 'to be deleted\n' > "$1/docs/old.md"
}

seed_base() {
    local d="$1"
    mkdir -p "$d/src/deep/nested" "$d/docs"
    seed_lua_whose_deleted_line_diffs_as_triple_dash "$d"
    seed_nested_file_that_git_interleaves_between_siblings "$d"
    seed_file_to_rename_with_an_edit "$d"
    seed_file_to_delete "$d"
}

seed_changes() {
    local d="$1"

    cat > "$d/src/one.lua" <<'EOF'
-- one.lua
local function greet(name)
  -- a comment starting with two dashes, so deleting it renders as `--- …`
  return "hello, " .. name .. "!"
end

-- A line several hundred characters long, for soft-wrap and horizontal scroll —
-- a pane that truncates and a pane that wraps look identical without one.
local long = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

-- Non-ASCII in a changed line. Lua's `#` counts BYTES, so every column computed
-- from it is short here: "Rosé Pine" measures 10 and pads one column narrow.
-- This is the fixture that proves a pane measured with utf8.len.
local theme = "Rosé Pine ── ╭╮ é"

return greet, long, theme
EOF

    cat > "$d/src/deep/nested/two.rs" <<'EOF'
fn main() {
    println!("two");
    println!("three");
}
EOF

    printf 'brand new\n' > "$d/src/added.txt"
    git -C "$d" rm -q docs/old.md
    git -C "$d" mv docs/notes.md docs/renamed.md
    printf 'second\n' >> "$d/docs/renamed.md"
}

seed_diff_past_max_diff_bytes() {
    FILES=400 LINES=400 DIR="$1" MARK="$2" python3 - <<'PY'
import os
files, lines = int(os.environ["FILES"]), int(os.environ["LINES"])
root, mark = os.environ["DIR"], os.environ["MARK"]
for f in range(files):
    d = os.path.join(root, "pkg", f"mod{f // 20:02d}")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, f"file{f:03d}.txt"), "w") as fh:
        fh.write("".join(f"module {f} {mark} line {i} with a reasonable amount of text on it\n"
                          for i in range(lines)))
PY
}

git_init() {
    git -C "$1" init -q -b main
    git -C "$1" config user.email demo@thurbox.invalid
    git -C "$1" config user.name "thurbox demo"
}

commit_all() {
    git -C "$1" add -A
    git -C "$1" commit -qm "$2"
}

have_session() {
    fleet get task "$(tbx_fleet_task_name "$1")" >/dev/null 2>&1
}

take_back_worktree_left_by_deleted_task() {
    local repo="$1" dir="$2" branch="$3"
    git -C "$repo" worktree remove --force "$dir" >/dev/null 2>&1 || true
    rm -rf "$dir"
    git -C "$repo" worktree prune >/dev/null 2>&1 || true
    if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
        git -C "$repo" branch -D "$branch" >/dev/null 2>&1 || true
        skip "took back $branch from a previous seed"
    fi
}

fresh_worktree() {
    local repo="$1" dir="$2" branch="$3"
    take_back_worktree_left_by_deleted_task "$repo" "$dir" "$branch"
    git -C "$repo" worktree add -q -b "$branch" "$dir" main
    git -C "$dir" config user.email demo@thurbox.invalid
    git -C "$dir" config user.name "thurbox demo"
}

create_session() {
    tbx_fleet_session "$1" "$2" "$UNPROFILED_HARNESS_FLEET_RUNS_AS_A_PLAIN_SHELL" \
        || die "creating session '$1' failed"
}

stage_without_committing_so_diff_head_shows_adds_and_renames() {
    git -C "$1" add -A
}

DEMO="$REPOS/demo"
if [ -d "$DEMO/.git" ]; then
    skip "repository exists: $DEMO"
else
    log "creating $DEMO"
    mkdir -p "$DEMO"
    git_init "$DEMO"
    seed_base "$DEMO"
    commit_all "$DEMO" "base"
fi

if have_session demo-changes; then
    skip "session 'demo-changes' exists"
else
    log "creating session 'demo-changes' on demo/changes"
    wt="$REPOS/demo-changes"
    fresh_worktree "$DEMO" "$wt" demo/changes
    seed_changes "$wt"
    stage_without_committing_so_diff_head_shows_adds_and_renames "$wt"
    create_session demo-changes "$wt"
    log "  $(git -C "$wt" diff --stat HEAD | tail -1)"
fi

if have_session demo-clean; then
    skip "session 'demo-clean' exists"
else
    log "creating session 'demo-clean' on demo/clean (no changes, on purpose)"
    fresh_worktree "$DEMO" "$REPOS/demo-clean" demo/clean
    create_session demo-clean "$REPOS/demo-clean"
fi

if [ "$want_big" = "1" ]; then
    BIG="$REPOS/big"
    if [ -d "$BIG/.git" ]; then
        skip "repository exists: $BIG"
    else
        log "creating $BIG (400 files x 400 lines — this takes a moment)"
        mkdir -p "$BIG"
        git_init "$BIG"
        seed_diff_past_max_diff_bytes "$BIG" original
        commit_all "$BIG" "base"
    fi

    if have_session demo-big; then
        skip "session 'demo-big' exists"
    else
        log "creating session 'demo-big' on demo/big"
        wt="$REPOS/demo-big"
        fresh_worktree "$BIG" "$wt" demo/big
        seed_diff_past_max_diff_bytes "$wt" REWRITTEN
        stage_without_committing_so_diff_head_shows_adds_and_renames "$wt"
        create_session demo-big "$wt"
        log "  $(git -C "$wt" diff --no-color HEAD | wc -c) bytes of diff"
    fi
fi

log "seeded. repositories under $REPOS; the sessions start with the sandbox's \`fleet up\`"
