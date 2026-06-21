#!/usr/bin/env python3
"""Bump the Chocolatey package to a released version.

Usage: bump-nuspec.py <version> <chocolatey_dir_or_nuspec> <checksums.txt>

Sets <version> in thurbox.nuspec and $version/$url64/$checksum64 in
tools/chocolateyinstall.ps1 to match the freshly published release. The Windows
artifact's sha256 is read from the release `checksums.txt`
(`thurbox-v<version>-x86_64-pc-windows-msvc.zip`). Exits non-zero if that
checksum is missing or any template anchor is not found, so CI fails loudly
rather than packing a stale/partial package.
"""
import re
import sys
from pathlib import Path

TARGET = "x86_64-pc-windows-msvc"


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2

    version_tag, arg, checksums_path = sys.argv[1], sys.argv[2], sys.argv[3]
    version = version_tag.lstrip("v")

    base = Path(arg)
    nuspec = base if base.suffix == ".nuspec" else base / "thurbox.nuspec"
    install_ps1 = nuspec.parent / "tools" / "chocolateyinstall.ps1"

    checks = {}
    for line in Path(checksums_path).read_text().splitlines():
        parts = line.split()
        if len(parts) == 2:
            checks[parts[1]] = parts[0]

    fname = f"thurbox-v{version}-{TARGET}.zip"
    sha = checks.get(fname)
    if sha is None:
        print(f"error: checksum missing for: {fname}", file=sys.stderr)
        return 1

    # nuspec <version> (first occurrence — the metadata one)
    nt = nuspec.read_text()
    nt, n = re.subn(r"<version>[^<]*</version>", f"<version>{version}</version>", nt, count=1)
    if n != 1:
        print("error: no <version> found in nuspec", file=sys.stderr)
        return 1
    nuspec.write_text(nt)

    # install script $version / $url64 / $checksum64 (single-quoted literals)
    url = f"https://github.com/Thurbeen/thurbox/releases/download/v{version}/{fname}"
    it = install_ps1.read_text()
    repls = [
        (r"(\$version\s*=\s*)'[^']*'", rf"\g<1>'{version}'"),
        (r"(\$url64\s*=\s*)'[^']*'", rf"\g<1>'{url}'"),
        (r"(\$checksum64\s*=\s*)'[^']*'", rf"\g<1>'{sha}'"),
    ]
    for pattern, repl in repls:
        it, c = re.subn(pattern, repl, it, count=1)
        if c != 1:
            print(f"error: anchor not found in chocolateyinstall.ps1: {pattern}", file=sys.stderr)
            return 1
    install_ps1.write_text(it)

    print(f"Bumped chocolatey package to {version} ({fname})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
