#!/usr/bin/env bash
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
probes="$root/crates/thurbox/tests/fixtures/lua_types"

if ! command -v lua-language-server > /dev/null 2>&1; then
    printf 'lua-language-server not found — install it (see scripts/install-dev-tools.sh)\n' >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# lua-language-server resolves `require` and `workspace.library` against the workspace root.
cp -R "$root/ui/." "$work/"
cp "$root/.luarc.json" "$work/.luarc.json"
mkdir -p "$work/probes"
cp "$probes"/*.lua "$work/probes/"

# Only the --check_out_path report names the diagnostic code and is not line-wrapped.
report="$work/check.json"
lua-language-server --check "$work" \
    --configpath "$work/.luarc.json" \
    --checklevel=Warning \
    --logpath "$work/log" \
    --check_out_path "$report" > /dev/null 2>&1 || true

if [ ! -s "$report" ]; then
    printf 'no findings at all — every probe type-checked clean, so the\n' >&2
    printf 'definitions in ui/lib/thurbox.d.lua are not being loaded.\n' >&2
    exit 1
fi

codes_for() {
    awk -v want="probes/$1\"" '
        /^    "file:/ { inside = (index($0, want) > 0); next }
        inside && /"code"/ { gsub(/[",]/, ""); print $2 }
    ' "$report"
}

expect() {
    local file=$1 code=$2
    if ! codes_for "$file" | grep -qx "$code"; then
        printf 'probes/%s: expected a %s finding, got none —\n' "$file" "$code" >&2
        printf '  thurbox.d.lua no longer describes what this probe misspells.\n' >&2
        return 1
    fi
    printf 'probes/%s: %s\n' "$file" "$code"
}

failed=0
expect prop.lua missing-fields || failed=1
expect command.lua missing-fields || failed=1
expect theme.lua undefined-field || failed=1

if grep -q '"file:.*/\(lib\|plugins\)/' "$report"; then
    printf 'the bundled interface reported a finding — run "just lint"\n' >&2
    failed=1
fi

exit "$failed"
