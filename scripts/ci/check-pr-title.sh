#!/usr/bin/env bash
#
# Verify a pull request title is the conventional commit squash merge will land.
#
# Under squash merge the commit on main is built by GitHub from the PR title
# plus its own " (#N)" suffix. That string — not any commit on the branch — is
# what `cog bump --auto` reads for the release decision and what the changelog
# quotes. Nothing else validates it: branch commits are discarded by the squash,
# so CI no longer walks them. This is the only gate between a typo in a title
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
# checked.
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

# `cog verify` names only the token it disliked — "Commit scope `program` not
# allowed" — never the set it was checked against, so an author who has not
# read cog.toml cannot correct the title from the failure alone. Quote the
# declared sets back. Each awk reads from the key to the line that closes it,
# so a reformatted (multi-line) array still prints; an allowlist the file does
# not declare prints nothing rather than an empty label.
print_allowlists() {
    [ -f cog.toml ] || return 0
    local types scopes
    types=$(awk '/^\[commit_types\]/{f=1;next} f&&/^\[/{exit} f&&/^[a-z]/{print $1}' \
        cog.toml | tr '\n' ' ' | sed 's/[[:space:]]*$//')
    scopes=$(awk '/^[[:space:]]*scopes[[:space:]]*=/{f=1} f{printf "%s", $0} f&&/\]/{exit}' \
        cog.toml | sed 's/.*\[//; s/\].*//' | tr -d '" ' | tr ',' ' ')
    [ -n "$types" ] && printf '  allowed types:  %s\n' "$types" >&2
    [ -n "$scopes" ] && printf '  allowed scopes: %s\n' "$scopes" >&2
    return 0
}

if ! report=$(printf '%s\n' "$subject" | cog verify --file - 2>&1); then
    printf 'The pull request title is not a conventional commit.\n\n' >&2
    printf '  title:    %s\n' "$title" >&2
    printf '  lands as: %s\n\n' "$subject" >&2
    printf '%s\n\n' "$report" >&2
    print_allowlists
    printf '\nA scope is optional; these are declared in cog.toml.\n' >&2
    exit 1
fi

printf 'Pull request title lands as a conventional commit: %s\n' "$subject"
