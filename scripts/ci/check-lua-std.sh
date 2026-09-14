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
# The two probes are what notices:
#
#   reads.lua  every field on those tables, as a plain dotted path. Must lint
#              CLEAN, so a table that regresses to a bare property fails here.
#   typos.lua  one misspelling per table. Each must still be reported, so the
#              fix cannot be a wildcard that accepts whatever is asked for.
#
# This covers those five tables, not everything `LuaHost::publish` serves — a path
# stops being checked at the first `[…]`, so a list has nothing below it to probe.
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

failed=0

if ! findings=$(selene --config selene.toml --quiet --no-summary "$probes/reads.lua" 2>&1); then
    printf 'tests/fixtures/lua_std/reads.lua: expected no findings, got\n' >&2
    printf '%s\n' "$findings" >&2
    printf '  thurbox.yml no longer declares a field the kernel publishes.\n' >&2
    printf '  Compare it with LuaHost::publish in src/kernel/host/.\n' >&2
    failed=1
else
    printf 'tests/fixtures/lua_std/reads.lua: clean\n'
fi

# Expected to fail, so the exit status carries no information — the messages do.
reported=$(selene --config selene.toml --quiet --no-summary "$probes/typos.lua" 2>&1 || true)

for table in thurbox.granted thurbox.platform thurbox.metrics thurbox.metrics.system thurbox.hover thurbox.preflight.mux; do
    if printf '%s\n' "$reported" | grep -qF "global \`$table\` does not contain"; then
        printf 'tests/fixtures/lua_std/typos.lua: %s rejects a misspelt field\n' "$table"
    else
        printf 'tests/fixtures/lua_std/typos.lua: expected %s to reject a\n' "$table" >&2
        printf '  misspelt field and it did not — thurbox.yml describes it too\n' >&2
        printf '  loosely to catch the typo it exists to catch.\n' >&2
        failed=1
    fi
done

exit "$failed"
