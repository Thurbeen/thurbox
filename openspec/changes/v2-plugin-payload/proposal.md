## Why

`v2-plugin-programs` made "a pane can hold `htop`, a REPL, a full-screen game" true.
Installing one is still not: `plugin install` delivers Lua and nothing else, so a
package that needs a binary or a data file ends in a README telling the user to go
and fetch things by hand.

The blocker is sharper than a policy: **a payload is not merely disallowed, it is
unrepresentable.** `extension_config::fetch_file` returns `Result<String, String>`,
and the remote half decodes curl's stdout with `String::from_utf8_lossy`. A 28.8 MB
data file does not fail to install — it installs corrupted. `validate_destination`
refusing anything but `.lua` is the only reason nobody has hit it.

The answer is not to plumb bytes through that seam and grow a manifest that
enumerates every file with a checksum and a platform matrix. It is that **a plugin
with a payload is a repository**, and `git clone` already does all of this: it
carries binaries, it preserves whatever directory structure the author chose, its
commit is a stronger integrity statement than a hand-maintained hash list, and it
already knows how to refuse to clobber a dirty working tree. git is a hard
dependency of thurbox already.

## What Changes

- **A git source is cloned, not fetched file-by-file.** `plugin install
  git+https://…/thurbox-doom` clones into `ui/<name>/`, and the whole repository is
  there — Lua in whatever layout it likes, plus binaries and data.
- **The clone keeps its `.git`.** That is what makes `update` a `fetch` rather than
  a re-download, and what makes an edit to an installed pane recoverable and
  protected: git refuses to overwrite a dirty tree, which it does better than the
  delivery matrix does.
- **`.git` becomes watcher noise.** `kernel::watch` watches the interface directory
  **recursively** and `is_noise` filters only editor scratch files, so anything
  inside a `.git` currently fires a reload. This is a **latent bug today**, not one
  this change introduces: it already affects anyone who runs `git init` in their
  interface directory to version their own panes, which is an entirely reasonable
  thing to do.
- **The loader loads spec-named panes.** `build` scans `plugins/*.lua` at the top
  level only, so a cloned repo's pane is invisible to it. The spec entry names the
  file (`file = "doom/plugins/40_doom.lua"`) and the loader loads it too. The load
  order still comes from the basename's numeric prefix, so nothing about ordering
  changes.
- **`thurbox.platform = { os, arch }` is published.** Platform selection stays out
  of the manifest entirely; the pane resolves its own path. A plugin knows things
  the kernel cannot model — a libc variant, a fallback to a binary already on
  `PATH`, a WASM build when there is no native one — and a snapshot field expresses
  all of them where a substitution template expresses one.
- **The fetch-files path stays Lua-only.** No bytes plumbing, no `sha256` field, no
  executable bit, no platform matrix. Payload arrives by clone; if you need one,
  ship a repository. Two mechanisms coexist, selected by the source's form — which
  is already how `resolve_source` works.
- **No size cap and no warning.** Installing a plugin is the user's decision and
  the size of what they asked for is theirs to know.
- **Nothing new runs.** An install writes whatever the repository contains,
  executable bits included, because stripping and re-adding them means fighting git
  for no gain. What gates *running* is unchanged: the `program` capability, granted
  per file by the user. The honest move is documenting that installing a plugin
  from a repository puts that repository's files on your disk — which is what
  cloning anything does.

## Capabilities

### New Capabilities

None. This is the same capability — acquiring an interface plugin — reached by a
second kind of source.

### Modified Capabilities

- `plugin-packages`: extended, with **ADDED** requirements rather than rewritten
  ones. Everything here is a second kind of source layered beside the first: what a
  git source delivers, what convergence and pin-advancing mean for a clone (git
  owns the working tree, not the delivery matrix), and that a pane may live outside
  `plugins/`. The fetch-files path's requirements are untouched and keep meaning
  exactly what they meant — which is why none of this is a `MODIFIED` delta.
  (`plugin-packages` is also not archived yet, so there is no base spec to restate
  under one.)

## Impact

- **`src/git/mod.rs`** — a `clone`/`fetch`/`checkout` trio built on the existing
  `git_command` helper, which already handles the local and remote-host forms.
- **`src/kernel/packages.rs`** — a git source kind beside the fetch path; `install`,
  `sync`, `update` and `remove` grow a clone branch. The lock records the resolved
  **commit**, which is what makes a spec reproducible.
- **`src/agent/extension_config.rs`** — `resolve_source_in` learns the git forms
  (`git+<url>`, a `.git` suffix, `git@host:path`). Deliberately explicit: no forge
  allowlist and no guessing from a bare `https://` URL, which already means
  "fetch files from this base".
- **`src/session/plugin_spec.rs`** — a spec entry may name a `file` outside
  `plugins/`; `validate_destination` keeps refusing traversal and absolute paths.
- **`src/kernel/host.rs`** — `build` loads spec-named panes; `thurbox.platform` is
  published.
- **`src/kernel/watch.rs`** — `.git` is noise.
- **`thurbox.yml`** — `thurbox.platform` declared, so a plugin reading it lints.
- **Docs** — `docs/PLUGINS.md`, `ui/AGENTS.md` (the file an agent reads before
  installing anything), `ui/README.md`, `docs/CONFIG.md`'s file table, `CLAUDE.md`,
  and the website's interface page.
- **No schema change.** The clone is a directory in the interface tree and the
  commit is recorded in the lock; nothing about it belongs in SQLite.
