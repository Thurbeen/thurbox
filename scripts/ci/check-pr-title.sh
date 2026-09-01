#!/usr/bin/env bash
#
# Verify a pull request title is the conventional commit squash merge will land.
#
# Under squash merge the commit on main is built by GitHub from the PR title
# plus its own " (#N)" suffix. That string — not any commit on the branch — is
# what `cog bump --auto` reads for the release decision, what the changelog
# quotes, and what check-conventional-commits.sh then holds the history to.
# Nothing else validates it: that sibling script walks *branch* commits, and
# squash discards exactly those. This is the only gate between a typo in a title
# box and a main whose history no longer parses.
#
# The title is checked in its final form, suffix included, because that is the
# artifact — not the bare title the author typed.
#
# Usage: check-pr-title.sh <title> <pr-number>
set -euo pipefail

if [ "$#" -ne 2 ]; then
    printf 'usage: %s <title> <pr-number>\n' "${0##*/}" >&2
    exit 2
fi

title="$1"
number="$2"

if [ -z "$title" ]; then
    printf 'The pull request title is empty.\n' >&2
    exit 1
fi

# GitHub appends " (#N)" itself, so a title that already ends in one lands as
# "… (#12) (#34)". Only a *trailing* reference is rejected: a title that cites
# another pull request mid-sentence is a different thing and still passes.
if printf '%s' "$title" | grep -qE '[[:space:]]\(#[0-9]+\)[[:space:]]*$'; then
    printf 'The pull request title already ends in a (#N) reference:\n\n' >&2
    printf '  %s\n\n' "$title" >&2
    printf 'Squash merge appends " (#%s)" on its own — drop the one in the title.\n' \
        "$number" >&2
    exit 1
fi

# `cog verify` resolves the current git author before it parses anything and
# panics when user.name is unset — which is every fresh CI checkout, where
# nothing commits and so nothing configures an identity. The author is only
# printed back, never part of the verdict, so lend one through a throwaway HOME
# (libgit2 reads $HOME/.gitconfig) rather than writing into the repository being
# checked. Same reasoning, and same fix, as check-conventional-commits.sh.
if ! git config --get user.name >/dev/null 2>&1 ||
    ! git config --get user.email >/dev/null 2>&1; then
    borrowed_home=$(mktemp -d)
    trap 'rm -rf "$borrowed_home"' EXIT
    printf '[user]\n\tname = pr title checker\n\temail = checker@invalid\n' \
        >"$borrowed_home/.gitconfig"
    export HOME="$borrowed_home"
fi

# cog.toml is read from the working directory, so the commit-type and scope
# allowlists this repository declares are part of what is enforced here.
subject="$title (#$number)"

if ! report=$(printf '%s\n' "$subject" | cog verify --file - 2>&1); then
    printf 'The pull request title is not a conventional commit.\n\n' >&2
    printf '  title:    %s\n' "$title" >&2
    printf '  lands as: %s\n\n' "$subject" >&2
    printf '%s\n\n' "$report" >&2
    printf 'Valid types and scopes are declared in cog.toml.\n' >&2
    exit 1
fi

printf 'Pull request title lands as a conventional commit: %s\n' "$subject"
