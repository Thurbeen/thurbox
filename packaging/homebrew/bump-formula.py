#!/usr/bin/env python3
"""Bump a Homebrew formula to a released version.

Usage: bump-formula.py <version> <formula.rb> <checksums.txt>

Rewrites the formula's `version "..."` and each per-platform `sha256 "..."` to
match the freshly published release. The url lines use `#{version}`
interpolation, so each artifact's target triple is read from the url and the
real filename (`thurbox-v<version>-<target>.tar.gz`) is looked up in the
release `checksums.txt`. Exits non-zero if a url has no matching checksum, so
CI fails loudly rather than pushing a stale/partial formula.
"""
import re
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2

    version_tag, formula_path, checksums_path = sys.argv[1], sys.argv[2], sys.argv[3]
    version = version_tag.lstrip("v")

    checks = {}
    for line in Path(checksums_path).read_text().splitlines():
        parts = line.split()
        if len(parts) == 2:
            checks[parts[1]] = parts[0]

    text = Path(formula_path).read_text()
    text, n = re.subn(r'version "[^"]*"', f'version "{version}"', text, count=1)
    if n != 1:
        print("error: no `version` line found in formula", file=sys.stderr)
        return 1

    pattern = re.compile(
        r'(?P<url>url "[^"]*thurbox-v#\{version\}-(?P<target>[^"]+?)\.tar\.gz")'
        r'(?P<gap>\s*)sha256 "[0-9a-f]*"'
    )

    missing = []

    def repl(m: "re.Match[str]") -> str:
        fname = f"thurbox-v{version}-{m.group('target')}.tar.gz"
        sha = checks.get(fname)
        if sha is None:
            missing.append(fname)
            return m.group(0)
        return f'{m.group("url")}{m.group("gap")}sha256 "{sha}"'

    text, count = pattern.subn(repl, text)
    if missing:
        print(f"error: checksum missing for: {', '.join(missing)}", file=sys.stderr)
        return 1
    if count == 0:
        print("error: no `url`/`sha256` pairs found in formula", file=sys.stderr)
        return 1

    Path(formula_path).write_text(text)
    print(f"Bumped {formula_path} to {version} ({count} artifact(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
