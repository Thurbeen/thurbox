# Homebrew packaging

Thurbox ships a [Homebrew](https://brew.sh) formula that installs the
**prebuilt** release binaries (`thurbox` + `thurbox-cli`) from the GitHub
Release. It is distributed through a **tap**
([`Thurbeen/homebrew-thurbox`](https://github.com/Thurbeen/homebrew-thurbox)),
not homebrew-core.

```bash
brew install thurbeen/thurbox/thurbox
# or, equivalently:
brew tap thurbeen/thurbox
brew install thurbox
```

The canonical formula lives here at
[`Formula/thurbox.rb`](Formula/thurbox.rb); CI copies it into the tap on every
release. The `version`/`sha256` values committed here are a last-known-good
template — CI overrides them per release.

## Supported platforms

The formula only declares the platforms that have a published release
artifact:

| Platform | Release artifact |
| -------- | ---------------- |
| macOS arm64 (Apple Silicon) | `aarch64-apple-darwin` |
| Linux x86_64 | `x86_64-unknown-linux-musl` (static) |

Intel macOS (`x86_64-apple-darwin`) and aarch64 Linux have **no** release
binary, so `brew install` reports "no available formula" there. Use
[`scripts/install.sh`](../../scripts/install.sh) or build from source on those
platforms.

## Runtime dependencies

- `tmux` (>= 3.2) and `git` — declared as formula `depends_on`.
- A coding-agent CLI (claude-code, codex, gemini, opencode, aider, …) is
  user-supplied (mentioned in the formula `caveats`).

## Test locally

```bash
brew install --build-from-source ./Formula/thurbox.rb   # install the local formula
brew audit --strict --formula ./Formula/thurbox.rb      # lint the recipe
brew test thurbox                                       # run the formula test block
```

## Automated publishing (CI)

New releases publish to the tap **automatically**. The `publish-homebrew` job
in [`.github/workflows/cd.yml`](../../.github/workflows/cd.yml) runs after the
GitHub Release is created and:

1. downloads the release `thurbox-<version>-checksums.txt`,
2. runs [`bump-formula.py`](bump-formula.py) to set `version` and each
   per-platform `sha256` from those checksums, then
3. clones the tap repo and commits the updated `Formula/thurbox.rb`.

The job is a no-op if the formula is already current, and is skipped entirely
when the token secret is absent (e.g. on forks).

### One-time setup

1. **Create the tap repo** — a GitHub repo named `homebrew-thurbox` under the
   `Thurbeen` org (the `homebrew-` prefix is what makes `brew tap
   thurbeen/thurbox` work). A bare repo with a `Formula/` directory is enough;
   the first release populates `Formula/thurbox.rb`.

2. **Provide a write token.** Generate a personal access token (fine-grained,
   `contents: read & write` on `Thurbeen/homebrew-thurbox`) and store it as the
   `HOMEBREW_TAP_TOKEN` repository secret on the **main** thurbox repo:

   ```bash
   gh secret set HOMEBREW_TAP_TOKEN   # paste the token when prompted
   ```

After that, every release updates the tap automatically.

## Manual publishing / initial import

To seed or update the tap by hand:

```bash
# Bump the local template to a published release, then copy it into the tap.
curl -fsSL -o /tmp/checksums.txt \
  "https://github.com/Thurbeen/thurbox/releases/download/v<version>/thurbox-v<version>-checksums.txt"
python3 bump-formula.py v<version> Formula/thurbox.rb /tmp/checksums.txt

git clone https://github.com/Thurbeen/homebrew-thurbox.git
mkdir -p homebrew-thurbox/Formula
cp Formula/thurbox.rb homebrew-thurbox/Formula/thurbox.rb
cd homebrew-thurbox
git commit -am "Update to v<version>"
git push
```

Pick a `<version>` that has **published release assets** (the formula points at
release tarballs).
