## 1. The watcher fix, which stands alone

- [x] 1.1 Add `.git` to `kernel::watch`'s `is_noise`: any event whose path contains a `.git` component is bookkeeping, not an interface edit
- [x] 1.2 Tests: a change inside `.git` does not wake the watcher; an edit to a plugin, a `lib/` module and `layout.lua` still does. This is a fix on its own merits — it already affects anyone who versions their interface directory — so it lands first and independently

## 2. Telling a plugin what machine it is on

- [x] 2.1 Publish `thurbox.platform = { os, arch }` from `std::env::consts::{OS, ARCH}`
- [x] 2.2 Declare `thurbox.platform` in `thurbox.yml`, so a plugin reading it lints rather than finding a silent nil
- [x] 2.3 Tests: a plugin can read both fields; the values are the ones the binary was built for

## 3. Recognising a git source

- [x] 3.1 Add a git kind to source resolution: a `git+` prefix, a `.git` suffix, or the scp-like `git@host:path`. Everything else keeps its current meaning
- [x] 3.2 Strip the `git+` prefix from the URL actually handed to git
- [x] 3.3 Tests: each of the three forms resolves to git; a bare `https://…` URL, a filesystem path and a bare name emphatically do not — that last one is the regression that would silently reinterpret every existing install

## 4. Cloning

- [x] 4.1 Add `clone` / `fetch` / `checkout` / `rev_parse` / `is_dirty` to `git`, built on the existing `git_command` helper so the local and remote-host forms are inherited
- [x] 4.2 Make them non-interactive: reuse `shell::SSH_HARDENING_OPTS` so a clone needing credentials the environment does not provide **fails with a message** rather than hanging the interface on a passphrase prompt
- [x] 4.3 Clone shallow (`--depth 1`), at a ref when the spec pins one
- [x] 4.4 Fetch a recorded commit by id when it is not the tip, so a spec plus lock reproduces elsewhere
- [x] 4.5 Tests: the constructed command lines, including that hardening is present and depth is set — asserted on construction, since the suite has no network

## 5. Installing a repository

- [x] 5.1 Clone into `<interface dir>/<name>/` on install, deriving `<name>` from the source and refusing one that is not a single safe segment
- [x] 5.2 Refuse a destination directory that already holds something unmanaged, leaving it untouched
- [x] 5.3 Record the entry in the spec, and the **resolved commit** in the lock — not the ref asked for
- [x] 5.4 Report what was installed and where, and say plainly that a repository's files are now on disk
- [x] 5.5 Tests: an install records the commit rather than the ref; an occupied destination is refused and nothing is written or recorded

## 6. Loading a pane the spec names

- [x] 6.1 Let a spec entry's `file` name a path outside `plugins/`, keeping the refusal of traversal and absolute paths
- [x] 6.2 Load spec-named panes in `build`, in addition to `plugins/*.lua`, without loading them twice when one is already there
- [x] 6.3 Confirm the load order still comes from the basename's numeric prefix, so where a plugin came from does not change where it sits
- [x] 6.4 Confirm a `require` of a module inside a working copy resolves — it should need no change, since `require` already splits on every dot from the interface root
- [x] 6.5 Add spec-named panes to `sources()` so the inventory accounts for them; deliberately do **not** walk the rest of the working copy
- [x] 6.6 Tests: a pane at `<name>/plugins/NN_x.lua` loads, takes its order from `NN`, appears in the inventory with its origin, and its sibling modules are requirable; Lua in the working copy the spec does not name is not loaded as a pane

## 7. Converging, advancing, removing a clone

- [x] 7.1 `sync` clones a git entry the directory lacks, and reports it
- [x] 7.2 `sync` leaves a working copy at the recorded commit alone, reporting `current`, and is idempotent
- [x] 7.3 Refuse to move a **dirty** working copy, reporting it as kept rather than completing silently — git's own refusal, surfaced
- [x] 7.4 `update` fetches and advances a git entry only when asked, reporting the commit it came from; an entry already at the tip reports already-current
- [x] 7.5 `remove` deletes the working copy, the spec entry and the record, and needs no network
- [x] 7.6 Tests: each outcome, and specifically that a dirty working copy survives a `sync` and an `update`

## 8. Saying what installing means

- [x] 8.1 `docs/PLUGINS.md` — the git source forms, what a clone delivers, `thurbox.platform` and why platform selection is not in the manifest, and the sentence about a repository's files landing on disk
- [x] 8.2 `ui/AGENTS.md` — the git form beside the bare name, since this is the file read before anything gets installed, plus the one-line warning
- [x] 8.3 `ui/README.md` — `thurbox.platform` in the published-globals table
- [x] 8.4 `docs/CONFIG.md` — the interface directory now holds installed plugins' own directories
- [x] 8.5 `CLAUDE.md` — the source kinds, the clone destination, the `.git` watcher rule, and the loader change
- [x] 8.6 Website — the interface page's acquisition story
- [x] 8.7 State the licence consequence where publishing is described: a package shipping a GPL program obliges its own repository to carry corresponding source

## 9. Verification

- [x] 9.1 `just lint` and `just test` clean, including the architecture rules — `kernel` reaching `git` is already allowed, but the new call sites should be checked rather than assumed
- [x] 9.2 Install a real repository end to end in a sandbox: a pane in a nested directory, a non-Lua file beside it, `plugin check` passing, the pane drawing
- [x] 9.3 Confirm the negative cases by hand: a dirty working copy surviving `sync`; `.git` churn not reloading the interface; a bare `https://` URL still fetching files rather than cloning
