#!/usr/bin/env bash
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

# GitHub's squash merge appends " (#N)" itself, so only a trailing reference is rejected.
if printf '%s' "$title" | grep -qE '[[:space:]]\(#[0-9]+\)[[:space:]]*$'; then
    printf 'The pull request title already ends in a (#N) reference:\n\n' >&2
    printf '  %s\n\n' "$title" >&2
    printf 'Squash merge appends " (#%s)" on its own — drop the one in the title.\n' \
        "$number" >&2
    exit 1
fi

# `cog verify` panics without a git identity; libgit2 reads one from $HOME/.gitconfig.
if ! git config --get user.name >/dev/null 2>&1 ||
    ! git config --get user.email >/dev/null 2>&1; then
    borrowed_home=$(mktemp -d)
    trap 'rm -rf "$borrowed_home"' EXIT
    printf '[user]\n\tname = pr title checker\n\temail = checker@invalid\n' \
        >"$borrowed_home/.gitconfig"
    export HOME="$borrowed_home"
fi

subject="$title (#$number)"

# `cog verify` names only the token it rejected, never the allowed set.
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
