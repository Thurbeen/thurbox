#!/usr/bin/env bash
#
# Seed a sandbox with a repository that has something to look at.
#
# `just sandbox` gives you a working thurbox with NOTHING in it — no repos, no
# sessions, no changes. Every feature that is about a repository (the review
# pane, the file viewer, sync, fork, the creation flow's branch list) is
# therefore untestable there until you hand-build a git repo, create a session,
# and change files in its worktree. This does that, once, reproducibly.
#
# A session is a Fleet Task (ADR-41) over a worktree this script makes. A Fleet
# session row carries no base branch, so the review shows the worktree's
# UNCOMMITTED changes (`git diff HEAD`): the fixtures are left uncommitted.
#
# Usage:
#   scripts/dev/demo-repo.sh                  # seed the "default" profile
#   scripts/dev/demo-repo.sh --profile foo
#   scripts/dev/demo-repo.sh --fresh          # seed a throwaway sandbox
#   scripts/dev/demo-repo.sh --big            # add the large-diff repo too
#   scripts/dev/sandbox.sh --demo             # seed, then launch the TUI
#
# Idempotent PER THING, not per run: the repository and each session are checked
# separately, so a session you deleted is recreated while the repository it came
# from is left alone.
#
# It never touches your real config: everything lands under the sandbox root
# (`repos/` beside `thurbox-config/`), so `--clean` takes it with the profile and
# `--fresh` gets a new one. Deliberately NOT under the config dir, where the
# interface's own inventory would find it.

set -euo pipefail

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
        -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

# Enter the sandbox unless a caller (sandbox.sh --demo) already has. Sourced
# either way, for the Fleet helpers; sourcing resets TBX_SANDBOX_ROOT, so an
# inherited one is put back.
inherited_root="${TBX_SANDBOX_ROOT:-}"
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib/sandbox-env.sh"
if [ -n "$inherited_root" ]; then
    TBX_SANDBOX_ROOT="$inherited_root"
else
    tbx_sandbox_init "$mode" "$profile"
fi
tbx_sandbox_fleet
command -v fleet >/dev/null \
    || die "no fleet on PATH — \`cargo build --bin fleet --bin runtime-mux\` first"

REPOS="$TBX_SANDBOX_ROOT/repos"
mkdir -p "$REPOS"

# ── the agent ───────────────────────────────────────────────────────────────
#
# A seeded session must not launch a real coding agent: it would want
# credentials, take seconds, and burn tokens for a demo. Fleet runs a harness
# it has no profile for as a program of that name, so the harness `sh` is a
# shell, which is all a demo session needs to exist and hold a pane. thurbox's
# own create flow offers only the profiled harnesses; this script applies the
# Task itself.
HARNESS="sh"

# ── fixtures ────────────────────────────────────────────────────────────────
#
# Every file below is here because it catches something. A demo repo with one
# modified file demonstrates that a diff renders and tests nothing.

seed_base() {  # <dir>
    local d="$1"
    mkdir -p "$d/src/deep/nested" "$d/docs"

    # MODIFIED, and the line that gets deleted below begins `-- `, so it reaches
    # the parser as `--- …`. `session::review::apply_file_metadata` gates its
    # `---`/`+++` arms on `in_hunk` for exactly this: without the gate a removed
    # comment line is eaten as a file header and the file reports fewer removals
    # than it has. Nothing else in a plain fixture reaches that branch.
    cat > "$d/src/one.lua" <<'EOF'
-- one.lua
local function greet(name)
  return "hello " .. name
end
return greet
EOF

    # MODIFIED, and nested deeper than its siblings. git emits files in its own
    # order, so `src/added.txt`, `src/deep/nested/two.rs` and `src/one.lua`
    # arrive interleaved — which is what catches a changed-files list that emits
    # a directory header whenever the directory changes (it prints `src/` twice).
    # Sorting by whole path does not fix it either: the nested file sorts BETWEEN
    # the two `src/` ones. A flat repo cannot catch this at all.
    cat > "$d/src/deep/nested/two.rs" <<'EOF'
fn main() {
    println!("one");
}
EOF

    # RENAMED-with-an-edit, below. The rename is the status people leave out, and
    # it is the only one that exercises `old_path` — a pane that never saw one
    # renders the arrow wrong and nobody notices until a real review.
    cat > "$d/docs/notes.md" <<'EOF'
# Notes
first
EOF

    # DELETED, below.
    printf 'to be deleted\n' > "$d/docs/old.md"
}

seed_changes() {  # <worktree>
    local d="$1"

    # The `-- ` line arrives as `--- …`; see seed_base.
    cat > "$d/src/one.lua" <<'EOF'
-- one.lua
local function greet(name)
  -- a comment starting with two dashes, so deleting it renders as `--- …`
  return "hello, " .. name .. "!"
end

-- A line several hundred characters long, for soft-wrap and horizontal scroll —
-- a pane that truncates and a pane that wraps look identical without one.
local long = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

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

    # ADDED.
    printf 'brand new\n' > "$d/src/added.txt"
    # DELETED.
    git -C "$d" rm -q docs/old.md
    # RENAMED, with an edit so it is a rename and not a pure move.
    git -C "$d" mv docs/notes.md docs/renamed.md
    printf 'second\n' >> "$d/docs/renamed.md"
}

# 400 files x 400 lines, entirely rewritten: ~20 MB, so it lands well past
# `kernel::diff::MAX_DIFF_BYTES` (4 MiB). Three things are unobservable without
# it, because a small repository answers faster than you can look: the `pending`
# state, the truncation flag, and any incremental work a pane does while reading.
# Unobservable means untested.
seed_big() {  # <dir> <marker>
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


# ── repositories and sessions ───────────────────────────────────────────────

git_init() {  # <dir>
    git -C "$1" init -q -b main
    git -C "$1" config user.email demo@thurbox.invalid
    git -C "$1" config user.name "thurbox demo"
}

commit_all() {  # <dir> <message>
    git -C "$1" add -A
    git -C "$1" commit -qm "$2"
}

have_session() {  # <name>
    fleet get task "$(tbx_fleet_task_name "$1")" >/dev/null 2>&1
}

# A fresh worktree of <repo> at <dir> on <branch>, made from main. A previous
# seed's worktree and branch are taken back first: deleting a Task leaves the
# directory it worked in, so re-seeding after a delete would otherwise hit
# "branch already exists" — and the session would never come back, which is the
# per-thing idempotency this script is supposed to have. Both belong to the
# demo, so both are ours to remove.
fresh_worktree() {  # <repo> <dir> <branch>
    local repo="$1" dir="$2" branch="$3"
    git -C "$repo" worktree remove --force "$dir" >/dev/null 2>&1 || true
    rm -rf "$dir"
    git -C "$repo" worktree prune >/dev/null 2>&1 || true
    if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
        git -C "$repo" branch -D "$branch" >/dev/null 2>&1 || true
        skip "took back $branch from a previous seed"
    fi
    git -C "$repo" worktree add -q -b "$branch" "$dir" main
    git -C "$dir" config user.email demo@thurbox.invalid
    git -C "$dir" config user.name "thurbox demo"
}

create_session() {  # <name> <dir>
    tbx_fleet_session "$1" "$2" "$HARNESS" || die "creating session '$1' failed"
}

# ── 1. the demo repository, and a session with changes in its worktree ──────
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
    # Staged, not committed: `git diff HEAD` then shows the added file too,
    # and the rename as a rename.
    git -C "$wt" add -A
    create_session demo-changes "$wt"
    log "  $(git -C "$wt" diff --stat HEAD | tail -1)"
fi

# ── 2. a session with NOTHING in its worktree ───────────────────────────────
#
# The kernel's own spec has a scenario for this ("an empty diff is reported,
# distinct from not-yet-known") and it cannot be demonstrated without a session
# that legitimately has nothing. It is also the one that catches a pane which
# renders "no changes" for a diff that has plenty.
if have_session demo-clean; then
    skip "session 'demo-clean' exists"
else
    log "creating session 'demo-clean' on demo/clean (no changes, on purpose)"
    fresh_worktree "$DEMO" "$REPOS/demo-clean" demo/clean
    create_session demo-clean "$REPOS/demo-clean"
fi

# ── 3. the large repository, on request ─────────────────────────────────────
if [ "$want_big" = "1" ]; then
    BIG="$REPOS/big"
    if [ -d "$BIG/.git" ]; then
        skip "repository exists: $BIG"
    else
        log "creating $BIG (400 files x 400 lines — this takes a moment)"
        mkdir -p "$BIG"
        git_init "$BIG"
        seed_big "$BIG" original
        commit_all "$BIG" "base"
    fi

    if have_session demo-big; then
        skip "session 'demo-big' exists"
    else
        log "creating session 'demo-big' on demo/big"
        wt="$REPOS/demo-big"
        fresh_worktree "$BIG" "$wt" demo/big
        seed_big "$wt" REWRITTEN
        git -C "$wt" add -A
        create_session demo-big "$wt"
        log "  $(git -C "$wt" diff --no-color HEAD | wc -c) bytes of diff"
    fi
fi

log "seeded. repositories under $REPOS; the sessions start with the TUI's \`fleet up\`"
