## 1. The reads the flow needs (`kernel::repos`)

- [x] 1.1 Add `kernel::repos::RepoStore` following `kernel::diff`: keyed, idempotent requests, one worker per request, `poll()` reporting whether anything landed
- [x] 1.2 Publish a host's remembered repositories as one flat list, most recent first, each row carrying path, name, parent, is_parent and is_git
- [x] 1.3 Synthesise a local parent's children into that list on a worker (v1's live scan), and take a remote parent's from the persisted child rows
- [x] 1.4 Serve a directory listing per (host, dir) with its git-ness marks, its loading state and its error
- [x] 1.5 Serve a base-branch list per (host, repo): fetch first (failure non-fatal), then list, ordered by v1's `ordered_branch_list` — local default first, `origin/<default>` above it
- [x] 1.6 Resolve `HostDef` from `hosts.toml` once and reuse it, so every request targets the right machine

## 2. The writes (`kernel::command`)

- [x] 2.1 Add `Command::Bookmark { host, path, action: add | remove | parent }`, parsed from a plugin's `(kind, opts)` with an unknown action refused
- [x] 2.2 Add: expand the tilde on the target machine, refuse a path that does not exist, record its git-ness, and touch recency
- [x] 2.3 Parent: persist the folder as a parent and replace its children from a scan, reporting how many were found
- [x] 2.4 Remove: drop the bookmark, and a parent's children with it
- [x] 2.5 Widen `Command::Create` with `extras` (path + worktree flag) and pass them plus `base` into `SpawnRequest`
- [x] 2.6 Skip the local `is_dir` check on create when a host is named

## 3. What plugins can read (`kernel::host`)

- [x] 3.1 Publish `thurbox.bookmarks`, `thurbox.browse` and `thurbox.branches` from the store, only for what is currently requested
- [x] 3.2 Publish hosts as `{ name, detail, backend }`, replacing the string list
- [x] 3.3 Publish the agent registry's default alongside the agent names
- [x] 3.4 Read the flow's requests off `store` in the loop and hand them to the store; mark the screen dirty when a result lands
- [x] 3.5 Let a float ask for absolute `cols`/`rows` as well as percentages, clamped to the screen

## 4. The flow (`ui/`)

- [x] 4.1 Add `ui/lib/textinput.lua`: a buffer with a cursor, printable insert, backspace/delete, left/right/home/end, and v1's readline chords through one dispatch point
- [x] 4.2 Add `ui/plugins/70_new_session.lua` as one floating plugin with a step machine, declaring `ctrl+n` globally (not passthrough, as in v1) and every in-flow key as data
- [x] 4.3 Host step: local plus each host with its detail, skipped entirely when there is no choice
- [x] 4.4 Repo step, list half: bookmark rows with `[ ]`/`[x]`, `[wt]`, `(dir)`, parent headers with `▸`/`▾`, indented children, scroll window and v1's footer hints
- [x] 4.5 Repo step, keys: `j`/`k`, `space` select-or-collapse, `w` worktree (refused with a reason on a known non-repo), `d` forget, `/` search, `tab` to the input, `enter` to confirm
- [x] 4.6 Repo step, search: a filter bar with a match count, fuzzy-matched against the displayed path, expanding collapsed groups while it is active
- [x] 4.7 Repo step, path input: the field, the ghost completion derived from the cached listing, `enter` to add, `ctrl+p` to import a parent, `shift+tab` back to the list
- [x] 4.8 Repo step, browse dropdown: `tab` opens it, entries filtered by the typed prefix with git ones marked, `up`/`down` to move, `enter` to descend or choose, `esc` to close only the dropdown
- [x] 4.9 Branch step: the fetched list with a visible wait, defaults first, `esc` back out of the flow
- [x] 4.10 Name step, then worktree-branch step prefilled with a branch-safe form of the name; both refuse empty
- [x] 4.11 Agent step: every agent with the configured default preselected, skipped when there is one or none
- [x] 4.12 Confirm: issue one `create` carrying host, primary repo, extras, base, branch, name and agent — and nothing when the flow was cancelled
- [x] 4.13 Ship it: add both new files to `bundled.rs` and declare every newly published field in `thurbox.yml`

## 5. Proof

- [x] 5.1 The bookmark command: add refuses a missing path, remove takes a parent's children with it, parent import reports its count
- [x] 5.2 Branch ordering puts the remote default above the local default, and a fetch failure still yields the local list
- [x] 5.3 A create carrying extras reaches `SpawnRequest` with each member's worktree flag intact, and a host-named create does not stat locally
- [x] 5.4 The flow plugin renders every step against a real snapshot, and offers every choice the kernel exposes
- [x] 5.5 `ctrl+n` moves to `GLOBAL_CHORDS` in `tests/v2_keymap.rs` and `new_session` to the covered list in `tests/v2_parity.rs`, with both counts updated
- [x] 5.6 The bookmark-recency divergence recorded in `KNOWN_DIVERGENCES` with its count updated
- [x] 5.7 Lint clean: `selene ui` (0 warnings), `stylua --check ui`, clippy `--all-targets --all-features`, `cargo fmt --check`, `cargo deny`, and the architecture rules. `lua-language-server` is not installed outside the Nix shell, so CI is what runs that one

## 6. Documentation

- [x] 6.1 Update the bundled-pane count and the flow's description in `CLAUDE.md`, `docs/V2-KERNEL.md` and `docs/PLUGINS.md`
- [x] 6.2 Close parity-gap items #7, #8 and #9 in `openspec/changes/v2-parity-gaps/proposal.md`
- [x] 6.3 Record what implementing this got wrong under "Findings from implementing" in `design.md`
