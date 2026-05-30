# Arch Linux (AUR) packaging

Two packages are provided here:

| Directory      | AUR name      | What it does                                              |
| -------------- | ------------- | -------------------------------------------------------- |
| `thurbox/`     | `thurbox`     | Builds from the release **source** tarball with `cargo`. |
| `thurbox-bin/` | `thurbox-bin` | Installs the **prebuilt** x86_64 (musl) release tarball.  |

Both install two binaries — `thurbox` (TUI) and `thurbox-cli` — plus the
MIT `LICENSE`. They `provides`/`conflicts` each other, so only one can be
installed at a time.

## Why two packages

- `thurbox` is the portable, idiomatic choice (any arch `cargo` targets).
  Its `build()` exports `THURBOX_RELEASE_VERSION="v$pkgver"` so the TUI
  reports the real version — without it `build.rs` falls back to the
  `0.0.0-dev` marker baked into `Cargo.toml`.
- `thurbox-bin` is fast and needs no Rust toolchain, but is x86_64-only
  (no aarch64-Linux release artifact is published) and uses the
  statically-linked musl tarball.

> Note: upstream's `thurbox-cli --version` reports `0.0.0-dev` in both
> packages — that binary takes its version from clap's `CARGO_PKG_VERSION`
> rather than the build-time injected `THURBOX_VERSION`. The TUI status
> bar shows the correct version. This is an upstream quirk, not a
> packaging issue.

## Runtime dependencies

- `tmux` (>= 3.2) and `git` are required.
- The `thurbox` (source) package links the system `sqlite` — its
  `prepare()` drops rusqlite's vendored `bundled` SQLite so the binary
  uses `libsqlite3.so` (Arch discourages bundled libs, and the vendored
  static lib also fails to link under the default rust-lld). The
  `thurbox-bin` package is statically linked and needs no `sqlite`.
- A coding-agent CLI (claude-code, aider, opencode, …) is user-supplied
  and listed as `optdepends`.

## Build & test locally

```bash
cd thurbox          # or thurbox-bin
makepkg -f          # build the package
makepkg -si         # build and install
namcap PKGBUILD     # lint the recipe (if namcap is installed)
namcap *.pkg.tar.zst
```

## Bumping to a new release

1. Set `pkgver` to the new version (drop the `v` prefix) and reset
   `pkgrel=1` in the relevant `PKGBUILD`.
2. Refresh checksums: `updpkgsums`.
3. Regenerate the metadata: `makepkg --printsrcinfo > .SRCINFO`.
4. Rebuild to confirm: `makepkg -f`.

Pick a `pkgver` that has **published release assets** (check the GitHub
releases page) — some tags exist without a completed binary release.

## Publishing to the AUR

The AUR holds each package in its own git repo; this directory is just
the source of truth. To publish/update:

```bash
git clone ssh://aur@aur.archlinux.org/thurbox.git aur-thurbox
cp thurbox/PKGBUILD thurbox/.SRCINFO aur-thurbox/
cd aur-thurbox
git commit -am "upgpkg: thurbox <version>"
git push
```

Repeat with `thurbox-bin/` against the `thurbox-bin.git` AUR repo.
