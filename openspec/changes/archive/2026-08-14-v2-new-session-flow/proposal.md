## Why

v2 can manage sessions it inherits but cannot make one: the bundled interface was
cut back to three panes and the creation flow went with it, so `ctrl+n` resolves
to nothing (`tests/v2_keymap.rs::CHORDS_AWAITING_THEIR_PANE`) and the session
list's empty state deliberately advertises no chord. Bringing back the flow it
had is not enough either — that flow offered **one repository, no bookmarks and
no base branch**, because `v2-session-flows` named multi-repo creation and remote
branch listing as explicit non-goals. v1's picker is where the choices actually
live: host-scoped bookmarks, parent folders, multi-select with a per-repo
worktree toggle, a path input that completes and browses, and a fetched
base-branch list. Anything less means a v2 user must go back to v1 to start work,
which is what `v2-retire-v1` is waiting on (parity-gap items #7, #8, #9).

## What Changes

- The **new-session flow returns as a bundled floating plugin**
  (`ui/plugins/70_new_session.lua`), reproducing v1's wizard step for step: host
  picker → repo picker → base branch → session name → worktree branch → agent.
- The **repo picker gains everything v1's has**: host-scoped bookmarks sorted by
  recency, parent-folder headers with collapsible children, `space` multi-select,
  `w` per-repo worktree toggle, a dim `(dir)` marker for a non-git member, `/`
  fuzzy search, `d` bookmark delete, a path input with inline completion, a
  browse dropdown that lists sub-directories and marks git ones, and `ctrl+p`
  parent import.
- The kernel gains the **reads that flow needs**, each served from a worker and
  published like any other read: repo bookmarks (with their scanned children),
  directory listings for the browse dropdown, and a base-branch list produced by
  v1's fetch-then-order pipeline.
- The kernel gains a **`bookmark` command** (add, remove, import-parent) so
  bookmark memory is written by an explicit user action, validated off the render
  path — a typed path that does not exist is refused before any git work starts.
- **`create` carries the whole choice**: additional repositories, each either
  taking its own worktree on the shared branch or attached as-is, plus the base
  branch. It no longer stats the repo path locally when a host is named.
- Hosts are published with the detail v1's picker shows (`ssh me@devbox`), and
  the agent registry's **default** is published so the agent step can preselect
  it. **BREAKING** for a third-party plugin reading `thurbox.hosts`: entries
  become tables, not strings.

## Capabilities

### New Capabilities

- `session-creation-flow`: how a user chooses what a new session is made of —
  host, repositories, worktree mode, base branch, session and branch names,
  agent — and what the kernel must expose so every one of those choices can be
  made and remembered without the interface blocking.

### Modified Capabilities

None. `session-creation` (from `v2-session-flows`) has no archived main spec to
delta against — it is still a delta in that change — so the additions to the
create command are specified here and fold in at archive time, the same way
`v2-session-flows` folded in `plugin-host-api`.

## Impact

- **Kernel**: new `src/kernel/repos.rs` (bookmarks, listings, branch lists — one
  more worker-backed store, following `kernel::diff`); `Command::Bookmark` and a
  wider `Command::Create` in `src/kernel/command.rs`; new published tables in
  `src/kernel/host.rs`; request plumbing plus one store in
  `src/bin/thurbox2.rs`.
- **Interface**: `ui/plugins/70_new_session.lua` (new, bundled — so
  `src/kernel/bundled.rs` ships it), a text-input helper in `ui/lib/`, and
  `thurbox.yml` extended with every newly published field so the sandbox stays
  statically checkable.
- **Reuse, not reimplementation**: `storage::repo_bookmarks`,
  `git::{list_dir_entries_on, classify_path_on, scan_child_repos_on,
  list_branches_on, default_branch_on, git_fetch_on}` and
  `session_ops::spawn::SpawnRequest.extra_repos` all exist and are used as they
  are.
- **Tests**: `ctrl+n` moves from `CHORDS_AWAITING_THEIR_PANE` to `GLOBAL_CHORDS`
  (`tests/v2_keymap.rs`) and `new_session` from `AWAITING_THEIR_PLUGIN` to the
  covered list (`tests/v2_parity.rs`); new coverage for the bookmark command, the
  branch ordering and the multi-repo create.
- **Docs**: the bundled-pane count in `CLAUDE.md`, `docs/V2-KERNEL.md` and
  `docs/PLUGINS.md`; parity-gap items #7, #8, #9 close.
