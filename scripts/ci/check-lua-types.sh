#!/usr/bin/env bash
#
# Prove `ui/lib/thurbox.d.lua` still catches the mistakes it exists to catch.
#
# `lua-language-server --check ui` only proves the bundled panes are clean,
# which a definitions file describing nothing would also achieve. These probes
# are the other direction: three panes that each make one of the silent failures
# the plugin API allows — a node prop the kernel drops (`convert.rs` ignores an
# unknown key), a command option no verb reads (`command/mod.rs` reads a fixed
# list), and a theme role no palette defines (`theme.lua`'s `__index` answers
# nil) — and each must come back as a finding rather than as a pane that draws
# the wrong thing and says nothing.
#
# The probes run in a throwaway copy of `ui/` rather than in the tree, because
# lua-language-server resolves both `require("lib.…")` and the relative entries
# of `workspace.library` against the workspace ROOT — a probe checked from
# anywhere else would type-check against no definitions and pass for the wrong
# reason. Copying also keeps them out of `--check ui`, which must stay clean.
#
# Usage: check-lua-types.sh
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
probes="$root/tests/fixtures/lua_types"

if ! command -v lua-language-server > /dev/null 2>&1; then
    printf 'lua-language-server not found — install it (see scripts/install-dev-tools.sh)\n' >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp -R "$root/ui/." "$work/"
cp "$root/.luarc.json" "$work/.luarc.json"
mkdir -p "$work/probes"
cp "$probes"/*.lua "$work/probes/"

# Read the findings from the machine-readable report rather than the progress
# output: only it names the diagnostic code, and only it is not line-wrapped.
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

# The report is one JSON object of file -> findings, pretty-printed one key per
# line. `codes_for` cuts the block belonging to one file and lists its codes,
# which is all this needs and costs no JSON parser in CI.
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

# The bundled interface travels in the same workspace, so a finding there is a
# real regression. Reported here rather than tolerated, even though `just
# lint-lua` would catch it too.
if grep -q '"file:.*/\(lib\|plugins\)/' "$report"; then
    printf 'the bundled interface reported a finding — run "just lint"\n' >&2
    failed=1
fi

exit "$failed"
