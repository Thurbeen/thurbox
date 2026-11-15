# Packaging

Distribution packages for thurbox.

## Distribution packages

| Directory   | Channel                  | Installs                                  |
| ----------- | ------------------------ | ----------------------------------------- |
| `homebrew/` | Homebrew tap (macOS/Linux) | prebuilt release binaries — see [`homebrew/README.md`](homebrew/README.md) |
| `aur/`      | Arch Linux (AUR)         | source + prebuilt binary — see [`aur/README.md`](aur/README.md) |
| `chocolatey/` | Chocolatey (Windows)   | prebuilt release zip — see [`chocolatey/README.md`](chocolatey/README.md) |
| `winget/`   | winget (Windows)         | prebuilt release zip (portable) — see [`winget/README.md`](winget/README.md) |
| `../flake.nix` | Nix flake (Linux/macOS) | source build of any ref, not tied to a release — see [`../nix/package.nix`](../nix/package.nix) |

All but the flake are published automatically on each release by jobs in
[`.github/workflows/cd.yml`](../.github/workflows/cd.yml). For the
distro-agnostic one-liner installer, see
[`scripts/install.sh`](../scripts/install.sh).
