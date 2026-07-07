#!/usr/bin/env python3
"""Bump the winget manifests to a released version.

Usage: bump-manifests.py <version> <manifests_dir> <checksums.txt>

Sets PackageVersion across all three manifests, and InstallerUrl/InstallerSha256
(installer manifest) + ReleaseNotesUrl (locale manifest) to match the freshly
published release. The Windows artifact's sha256 is read from the release
`checksums.txt` (`thurbox-v<version>-x86_64-pc-windows-msvc.zip`) and emitted
uppercase (winget-pkgs validation normalizes to uppercase). Exits non-zero if
that checksum is missing or any template anchor is not found, so CI fails loudly
rather than submitting a stale/partial manifest set.
"""
import re
import sys
from pathlib import Path

TARGET = "x86_64-pc-windows-msvc"


def sub_once(text: str, pattern: str, repl: str, what: str) -> str:
    out, n = re.subn(pattern, repl, text, count=1)
    if n != 1:
        print(f"error: anchor not found: {what}", file=sys.stderr)
        raise SystemExit(1)
    return out


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2

    version_tag, manifests_dir, checksums_path = sys.argv[1], sys.argv[2], sys.argv[3]
    version = version_tag.lstrip("v")
    base = Path(manifests_dir)

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
    sha = sha.upper()

    version_manifest = base / "Thurbeen.thurbox.yaml"
    installer_manifest = base / "Thurbeen.thurbox.installer.yaml"
    locale_manifest = base / "Thurbeen.thurbox.locale.en-US.yaml"

    url = f"https://github.com/Thurbeen/thurbox/releases/download/v{version}/{fname}"
    notes = f"https://github.com/Thurbeen/thurbox/releases/tag/v{version}"
    pv = rf"(?m)^PackageVersion:\s*.*$"

    # Version manifest: PackageVersion only.
    vt = version_manifest.read_text()
    vt = sub_once(vt, pv, f"PackageVersion: {version}", "PackageVersion (version manifest)")
    version_manifest.write_text(vt)

    # Installer manifest: PackageVersion, InstallerUrl, InstallerSha256.
    it = installer_manifest.read_text()
    it = sub_once(it, pv, f"PackageVersion: {version}", "PackageVersion (installer manifest)")
    it = sub_once(
        it,
        r"(?m)^(\s*InstallerUrl:\s*).*$",
        rf"\g<1>{url}",
        "InstallerUrl",
    )
    it = sub_once(
        it,
        r"(?m)^(\s*InstallerSha256:\s*).*$",
        rf"\g<1>{sha}",
        "InstallerSha256",
    )
    installer_manifest.write_text(it)

    # Locale manifest: PackageVersion, ReleaseNotesUrl.
    lt = locale_manifest.read_text()
    lt = sub_once(lt, pv, f"PackageVersion: {version}", "PackageVersion (locale manifest)")
    lt = sub_once(
        lt,
        r"(?m)^(ReleaseNotesUrl:\s*).*$",
        rf"\g<1>{notes}",
        "ReleaseNotesUrl",
    )
    locale_manifest.write_text(lt)

    print(f"Bumped winget manifests to {version} ({fname})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
