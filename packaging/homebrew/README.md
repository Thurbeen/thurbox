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
- A coding-agent CLI (claude-code, codex, antigravity, opencode, aider, …) is
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
when the deploy-key secret is absent (e.g. on forks).

The push uses **SSH with a write-enabled deploy key** rather than a PAT: the
`Thurbeen` org blocks cross-repo personal access tokens, so a repo-scoped
deploy key is both the working credential and the least-privilege one (it can
write to the tap repo and nothing else).

### One-time setup

1. **Create the tap repo** — a GitHub repo named `homebrew-thurbox` under the
   `Thurbeen` org (the `homebrew-` prefix is what makes `brew tap
   thurbeen/thurbox` work). A bare repo with a `Formula/` directory is enough;
   the first release populates `Formula/thurbox.rb`.

2. **Add a write deploy key.** Generate a dedicated key, register the **public**
   half on the tap repo with write access, and store the **private** half as the
   `HOMEBREW_TAP_DEPLOY_KEY` secret on the **main** thurbox repo:

   ```bash
   ssh-keygen -t ed25519 -C "thurbox-release-ci@homebrew-tap" -f tap_key -N ""
   gh repo deploy-key add tap_key.pub --repo Thurbeen/homebrew-thurbox \
     --title thurbox-release-ci --allow-write
   gh secret set HOMEBREW_TAP_DEPLOY_KEY --repo Thurbeen/thurbox < tap_key
   rm -f tap_key tap_key.pub   # don't leave the private key on disk
   ```

   > Org-owned repos disable deploy keys by default. If `deploy-key add` reports
   > *"Deploy keys are disabled for this repository"*, an org owner must enable
   > them under **Org Settings → Repository → Repository deploy keys** first.

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
