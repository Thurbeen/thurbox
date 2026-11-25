#!/usr/bin/env bash
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
probes="$root/crates/thurbox/tests/fixtures/lua_std"

if ! command -v selene > /dev/null 2>&1; then
    printf 'selene not found — install it (see scripts/install-dev-tools.sh)\n' >&2
    exit 1
fi

# selene resolves `std` against the working directory, not the config's directory.
cd "$root"

selene_findings_ignoring_exit_status() {
    selene --config selene.toml --display-style Json2 "$1" 2>&1 || true
}

# `grep -c` exits 1 on no match.
count() {
    printf '%s\n' "$1" | grep -cF "$2" || true
}

failed=0

reported=$(selene_findings_ignoring_exit_status "$probes/reads.lua")
clean=$(count "$reported" '{"type":"Summary","errors":0,"warnings":0,"parse_errors":0}')
if [ "$clean" -eq 1 ]; then
    printf 'tests/fixtures/lua_std/reads.lua: clean\n'
else
    printf 'tests/fixtures/lua_std/reads.lua: expected a clean lint, got:\n' >&2
    printf '%s\n' "$reported" >&2
    printf '  Either thurbox.yml no longer declares a field the kernel publishes\n' >&2
    printf '  — compare it with LuaHost::publish in src/kernel/host/ — or selene\n' >&2
    printf '  never linted the probe.\n' >&2
    failed=1
fi

# Issue #1133: tables declared as bare properties rejected every dotted read while CI stayed green.
typo_probes_listed_so_a_missing_one_fails=(
    granted.lua platform.lua keyboard.lua metrics.lua metrics_system.lua hover.lua
    preflight_mux.lua settings_features.lua theme_roles.lua bookmarks.lua
    browse.lua branches.lua worktrees.lua
)

for name in "${typo_probes_listed_so_a_missing_one_fails[@]}"; do
    reported=$(selene_findings_ignoring_exit_status "$probes/typos/$name")
    diagnostics=$(count "$reported" '"type":"Diagnostic"')
    rejections=$(count "$reported" '"code":"incorrect_standard_library_use"')
    if [ "$rejections" -eq 1 ] && [ "$diagnostics" -eq 1 ]; then
        printf 'tests/fixtures/lua_std/typos/%s: the misspelt field is rejected\n' "$name"
    else
        printf 'tests/fixtures/lua_std/typos/%s: expected exactly one\n' "$name" >&2
        printf '  incorrect_standard_library_use and nothing else, got %s of it\n' "$rejections" >&2
        printf '  among %s finding(s):\n' "$diagnostics" >&2
        printf '%s\n' "$reported" >&2
        printf '  Either thurbox.yml describes that table too loosely to catch the\n' >&2
        printf '  typo this probe exists to catch, or selene never linted it.\n' >&2
        failed=1
    fi
done

exit "$failed"
