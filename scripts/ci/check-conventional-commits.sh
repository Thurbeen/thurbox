#!/usr/bin/env bash
#
# Verify a range's commit messages against the conventional-commit spec.
#
# `cog check` does this in one call and is what CI ran until the no-mistakes
# gate started authoring commits of its own. It commits the fixes its CI step
# writes under a subject baked into that binary — `no-mistakes: apply CI fixes`
# — which is not a conventional commit and which no repository setting can
# retemplate: `.no-mistakes.yaml`'s `commit.fix_message` governs only the
# per-step auto-fix commits. `cog check` has no way to exempt one commit, so
# the gate's own CI-fix commit failed the check it was pushed to fix and no
# further fix could ever land.
#
# Every other commit is held to exactly what `cog check` enforced: `cog verify`
# reads the same cog.toml, so the commit-type and scope allowlists still apply,
# and merge commits are skipped the way `ignore_merge_commits` skipped them.
set -euo pipefail

# The subjects the no-mistakes binary hardcodes for commits it authors itself.
# Matched whole, so a hand-written message that merely mentions one is checked.
GATE_SUBJECTS=(
    "no-mistakes: apply CI fixes"
    "no-mistakes: apply agent fixes"
)

# `cog verify` resolves the *current* git author before it parses anything and
# panics when `user.name` is unset — which is every fresh CI checkout, where
# nothing commits and so nothing configures an identity. `cog check` never
# needed one, so the rewrite to `verify` turned that into 1276 "non compliant"
# commits. The author is only printed back, never part of the verdict, so lend
# one through a throwaway HOME (libgit2 reads `$HOME/.gitconfig`) rather than
# writing into the repository being checked.
if ! git config --get user.name >/dev/null 2>&1 ||
    ! git config --get user.email >/dev/null 2>&1; then
    borrowed_home=$(mktemp -d)
    trap 'rm -rf "$borrowed_home"' EXIT
    printf '[user]\n\tname = conventional commit checker\n\temail = checker@invalid\n' \
        >"$borrowed_home/.gitconfig"
    export HOME="$borrowed_home"
fi

# Default to every commit reachable from HEAD, which is the range `cog check`
# walked: this repository's history starts at its first commit, not at a tag.
range="${1:-HEAD}"

non_compliant=0

while IFS= read -r sha; do
    subject=$(git log -1 --format=%s "$sha")
    for gate_subject in "${GATE_SUBJECTS[@]}"; do
        if [ "$subject" = "$gate_subject" ]; then
            continue 2
        fi
    done

    if ! report=$(git log -1 --format=%B "$sha" | cog verify --file - 2>&1); then
        printf 'Errored commit: %s\n\tCommit message: %s\n%s\n' \
            "$sha" "$subject" "$report" >&2
        non_compliant=$((non_compliant + 1))
    fi
done < <(git rev-list --no-merges "$range")

if [ "$non_compliant" -ne 0 ]; then
    printf 'Found %d non compliant commits in %s\n' "$non_compliant" "$range" >&2
    exit 1
fi

printf 'No errored commits in %s\n' "$range"
