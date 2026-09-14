#!/usr/bin/env bash
#
# Prove `thurbox.yml` still declares the fields the kernel publishes on the
# injected tables a plugin reads by name.
#
# `selene ui examples` proves the bundled interface is clean, which a standard
# library declaring nothing about those tables would also achieve — and did:
# `thurbox.granted`, `.platform`, `.metrics`, `.hover` and `.preflight.mux` were
# declared as bare properties, so a dotted read off any of them was an
# `incorrect_standard_library_use`, while CI stayed green because nothing in
# `ui/` or `examples/` reads them in a form selene can see (issue #1133).
#
# The probes are what notices:
#
#   reads.lua  every field on those tables, as a plain dotted path. Must lint
#              CLEAN, so a table that regresses to a bare property fails here.
#   typos/     one file per table, each misspelling one field. Each must still
#              be reported, so the fix cannot be a wildcard that accepts
#              whatever is asked for.
#
# Both halves are asserted on a positive result rather than on the absence of
# one, so a selene that never linted a probe — a renamed or deleted file — fails
# instead of passing quietly. That is why the loop names its probes instead of
# globbing, and why declaring a further table takes a file here *and* its name in
# that list.
#
# This covers those five tables, not everything `LuaHost::publish` serves — a path
# stops being checked at the first `[…]`, so a list has nothing below it to probe.
#
# Read through selene's `Json2` display style, and asserted on the `code` field.
# That code is selene's own lint identifier — the same name `selene.toml`'s
# `[lints]` keys and a `-- selene: allow(…)` comment use — so it is a contract
# rather than prose. An earlier version grepped the message text ("does not
# contain the field"), which a selene release could reword into a gate that
# passes every probe while catching nothing. Which table a finding belongs to
# comes from the file it was found in, not from that text, which is why `typos/`
# is a file per table.
#
# Run from the repository root: selene resolves the `std` name against the
# working directory, not the directory of the config it was given, so the `cd`
# below is what makes `selene.toml`'s `std = "thurbox"` find this repository's
# `thurbox.yml`.
#
# Usage: check-lua-std.sh
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
probes="$root/tests/fixtures/lua_std"

if ! command -v selene > /dev/null 2>&1; then
    printf 'selene not found — install it (see scripts/install-dev-tools.sh)\n' >&2
    exit 1
fi

cd "$root"

# One JSON object per line, so counting lines counts findings and no JSON parser
# is needed in CI — the same reason `check-lua-types.sh` reads its report with
# awk. A probe is expected to fail, so selene's exit status carries no
# information here and is discarded; the objects are the answer.
lint() {
    selene --config selene.toml --display-style Json2 "$1" 2>&1 || true
}

# `grep -c` exits 1 on no match, which `set -e` would take as a failure.
count() {
    printf '%s\n' "$1" | grep -cF "$2" || true
}

failed=0

reported=$(lint "$probes/reads.lua")
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

for name in granted.lua platform.lua metrics.lua metrics_system.lua hover.lua preflight_mux.lua; do
    reported=$(lint "$probes/typos/$name")
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
