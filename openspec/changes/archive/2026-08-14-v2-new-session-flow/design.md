## Context

See `proposal.md` — Why. What shapes the approach is the kernel's five rules
(`docs/V2-KERNEL.md`): reads are snapshots, writes are commands, Lua never
blocks, capabilities are absent rather than blocked, and anything touching the
world runs on a worker. v1's picker breaks all five if ported literally: it
stats paths on the UI thread, opens a modal *from a worker callback*
(`poll_branch_list`), holds a `HashMap` listing cache inside the modal, and
reaches the database directly for bookmarks.

Three things already exist and are reused unchanged: `storage::repo_bookmarks`
(host-scoped rows with `is_parent` / `is_git` / `parent_path`, schema v39), the
`git::*_on(host, …)` family the v1 flow calls, and
`session_ops::spawn::SpawnRequest.extra_repos`, which the headless
`--add-repo`/`--add-dir` path already drives. So this change is mostly about
**where** that work runs and **how** its results reach Lua.

## Goals / Non-Goals

**Goals:**

- Every choice v1's wizard offers, offered by a plugin, with no blocking call in
  the render path.
- One store for the flow's three worker-backed reads, following
  `kernel::diff`'s shape so the fifth rule stays a pattern rather than a habit.
- Bookmark memory written only by explicit user acts, and validated before it is
  written.

**Non-Goals:**

- Reworking creation itself. `spawn_session_headless` stays the one pipeline;
  this change only widens what the command hands it.
- A general filesystem capability for plugins. Listings arrive as *published
  results of a request*, keyed by (host, dir) — the plugin still cannot open a
  path.
- v1's per-keystroke local path completion as such (see D4).
- Fork/restart/sync/restore, which `v2-session-flows` already delivered.

## Decisions

### D1 — Requests travel through `store`; results are published reads

A plugin cannot call a function that waits, and a command cannot return a
value — so a *parameterised read* (list this directory, list this repo's
branches) has no obvious home. The flow writes what it wants into the shared
`store` (`store.want_browse = "<host>\0<dir>"`, `store.want_branches`,
`store.want_bookmarks`); the loop reads those with `LuaHost::shared_string` each
iteration and asks a new `kernel::repos::RepoStore` for them. Requests are
**keyed and idempotent** — asking every frame costs a hash lookup after the
first — and results are published back as ordinary tables
(`thurbox.browse`, `thurbox.branches`, `thurbox.bookmarks`).

*Alternatives considered.* (a) New command kinds (`browse`, `branches`) whose
results land somewhere: rejected — a command is an at-most-once *act*, and
issuing one per keystroke to re-filter a directory abuses the in-flight list the
session pane draws. (b) Granting `files.list` an arbitrary root: rejected — it
is rooted at a session's directory on purpose, and the flow needs paths on
another *machine*, which no filesystem capability would cover.

*Precedent.* This is exactly how the kernel already drives diffs
(`DiffStore::request` per focused session) and how the session list already
publishes its selection back to the kernel (`store.selected`, read by the loop).

### D2 — Bookmark writes are commands; "select what I just added" is recency

Adding, importing and forgetting are explicit user acts with side effects, so
they are `Command::Bookmark { host, path, action }` — dispatched on the bus, run
on a worker with their own connection, reported through the same in-flight error
channel as everything else. Validation (tilde expansion, existence, git-ness,
child scan) happens there, so a typo is refused before any git work starts, as
the spec requires.

That leaves a gap: the *expanded* path is only known on the worker (a remote `~`
needs a round trip), so the plugin cannot select the row it just asked for by
name. Rather than invent a result channel, bookmarks are published **most
recently used first** and an add *touches* recency — so "the row I just added or
re-added" is "the first row", which is precisely v1's select-or-add semantics.
The plugin arms a one-shot `select_newest` when it issues the add.

*Alternative considered.* A `detail` field on the in-flight command carrying the
expanded path, read during the linger window. Rejected: it makes a plugin's
correctness depend on a sweep interval.

*Accepted divergence.* v1 loads bookmarks once when the picker opens and appends
new rows at the end; v2 re-reads them and a touched row moves to the top. Same
selection, different position — recorded rather than hidden.

### D3 — One flat row shape for bookmarks; the store hides v1's asymmetry

v1 builds picker rows from two different sources depending on the target: a
**local** parent live-scans its children on every open (instant, ephemeral),
while a **remote** parent uses the children persisted at import time (a re-scan
would be an ssh round trip per open). That asymmetry is worth keeping — and
worth keeping *out* of Lua. `RepoStore` publishes one flat list per host, each
row `{ path, name, parent, is_parent, is_git }`, with local parents' scanned
children synthesised into it on a worker. The plugin builds headers, indentation
and collapse from `parent`/`is_parent` alone and never knows which source a row
came from.

### D4 — Inline completion is derived from the listing, not from the filesystem

v1 computes a ghost suggestion per keystroke via `paths::complete_directory_path`
— a synchronous readdir on the UI thread, and wrong for a remote target (it
suppresses the suggestion entirely there). Here the directory component's
listing is already being requested and cached for the browse dropdown, so the
suggestion is the common completion of the entries matching the typed prefix,
computed in Lua from a table it already has. Same appearance, no filesystem in
the render path — and it now works for a remote target, which v1's cannot.

### D5 — A branch list is a keyed read with a visible loading state

v1's worker *opens the branch selector when it lands* (`poll_branch_list`), which
v2 has no way to express: nothing outside Lua may decide which step the flow is
on. So the flow advances to its branch step immediately and renders
`thurbox.branches.loading` until rows arrive — which is strictly better than v1,
where the gap between the repo picker closing and the selector opening showed
nothing but a placeholder row. Ordering is v1's `ordered_branch_list` moved into
`kernel::repos` verbatim (local default first, `origin/<default>` pinned above
it), and a failed fetch is non-fatal, as it is in v1.

### D6 — `create` carries every member; the plugin issues one command

`Command::Create` gains `extras: Vec<ExtraRepo>` and passes `base` through to
`SpawnRequest`. One command per session, not one per repository: the pipeline
already builds each extra worktree on the shared branch and rolls the whole
thing back on failure, and a session half-created by three commands has no
owner. The local `is_dir` check is skipped when `host` is named — it currently
stats a *remote* path on the local machine and refuses a perfectly good repo.

### D7 — Hosts are published as tables

The picker must show what distinguishes two hosts (`HostDef::picker_detail` —
`ssh me@devbox`, `wsl Ubuntu-22.04`), and the flow must send the backend name
(`ssh:devbox`) the create command expects. Publishing `{ name, detail, backend }`
per host replaces the string list rather than adding a parallel table keyed by
name, because two parallel lists that must stay aligned is the bug this avoids.
`thurbox.yml` is updated in the same commit, so a plugin reading the old shape
fails lint rather than rendering `nil`.

### D8 — Text editing lives in `ui/lib/textinput.lua`

The `input` node renders a value and a cursor; it owns no buffer, which is
correct — a reload must not lose what you typed, and `state` is what survives a
reload. So one library module owns the buffer and the key handling: printable
insert, backspace/delete, left/right/home/end, and the readline chords v1
supports in its own fields (`ctrl+a/e/b/f/h/d/w/u/k`) via one dispatch point,
mirroring `modals::apply_ctrl_line_edit`. Four text fields in this flow use it,
and every later pane that needs a field gets it for free.

### D9 — The whole wizard is one plugin, not one plugin per step

Six steps could be six floating plugins. They are one, for two reasons. The
kernel renders **every** floating plugin each frame purely to discover it is not
floating (`docs/V2-KERNEL.md`, "Stop probing closed modals"), so six closed
modals would cost six Lua calls a frame forever. And the steps share one piece
of state — the pending choice — which would otherwise live in `store` as a
protocol between plugins that only ever run in sequence.

### D10 — Float sizing gains absolute cells

v1's picker is a fixed 60 columns with its height fitted to its content
(`centered_fixed_height_rect`). A float can only ask for a percentage of the
screen, so the same modal would grow with the terminal and its rows would drift
from the content they frame. `Float` therefore also accepts `cols`/`rows`
(absolute, clamped to the screen) alongside the existing percentages — one field
each, no new node kind, and the flow computes its own height exactly as v1 does.

*Alternative considered.* Keeping percentages and recording a divergence.
Rejected: the divergence would be in every screenshot of the flow, and the
primitive that fixes it is smaller than the note explaining it.

## Risks / Trade-offs

**A result outliving the step that asked for it.** v1 guards this with a
generation stamp on every worker result (`repo_picker_gen`). → Results here are
keyed by their *request* (host, dir) / (host, repo) and merely cached; the
plugin renders the one matching what it is currently asking for, so a late
arrival for an abandoned step is inert rather than needing to be dropped.

**Publish cost.** Bookmarks, listing and branches are three more tables built
per publish, and cost tracks node and table count. → Each is published only for
the host/dir/repo currently requested (a closed flow requests nothing, so all
three are empty), and `publish` already runs only before a paint or an input
dispatch.

**Bookmark writes race the snapshot.** A worker writes `repo_bookmarks` while
the UI thread holds its own connection. → Same-database contention is exactly
what the bus's per-command connection exists for; the flow's rows are re-read
by `RepoStore` on the command completing, not by polling.

**A wider `create` is a wider failure surface** (a bad extra path, a branch that
exists in one repo but not another). → Every failure still arrives through the
one in-flight error channel, and `spawn_session_headless` already rolls back the
worktrees it made.

**`thurbox.hosts` changes shape.** → It is documented as unstable
(`docs/PLUGINS.md`), the bundled interface is updated with it, and `thurbox.yml`
makes the old shape a lint error rather than a runtime `nil`.

**Legacy bookmarks with unknown git-ness.** v1 backfills them with a local
`.git` probe on the UI thread. → The store's worker fills `is_git` when it
scans, and an unknown row stays selectable with worktree mode allowed — the same
"unknown is permitted, creation reports the truth" stance v1 takes.

## Findings from implementing

**D10 was half wrong about which primitive was missing.** The design said v1's
modals are a fixed 60 *columns*. They are not: `centered_fixed_height_rect`
takes a percentage width and an absolute height, so the load-bearing half was
`rows`, not `cols`. Both were added — `cols` is one field and completes the pair
— but the flow uses `{ width = 60, rows = <sum of its children> }`, and the
height is summed from the children that were actually built rather than computed
a second time, so the frame cannot disagree with what is inside it.

**The renderer cannot leave anything behind for the key handler.** `state` is not
written during a render, so the ghost completion the frame drew was invisible to
the `tab` that accepted it — `tab` completed nothing. The fix is a shared pure
function (`suggestion_for`) called from both, which is also the only way the two
cannot drift: what `tab` inserts is by construction what is on screen. The same
trap will catch the next pane that derives something in `render` and reads it in
`on_key`.

**Lua's `and`/`or` cannot carry a miss.** `searching and fuzzy(query, path) or
{}` reads as "the match, or nothing" and is neither: a *miss* is `nil`, and `nil
or {}` is an empty table, which then reads as "matched" — so the search filtered
nothing at all while looking entirely correct. Spelled as a branch now, with the
reason recorded at the site. Worth knowing before writing the next filter.

**Selecting what you just added had to become a rule, not a lookup** (D2 held,
for a reason the design only half stated). Because the expansion happens on the
worker, the flow cannot name the row it asked for — and it also cannot consume
"the newest row" the moment the command finishes: the bookmark list is
invalidated in the same iteration, so the first publish after completion still
carries the *old* rows. The consumption is therefore gated on the write no longer
being in flight **and** the re-read having landed, which is two conditions where
the design implied one.

**The test harness had a hazard the production code does not.**
`paths::set_test_dir` is thread-local and every command runs on its own thread,
so a bookmark test using it wrote rows into the developer's real database (eight
of them, found and removed). Isolating a command test means the process-wide
`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`, which is what the worker reads. Recorded
here rather than only in the test, because any future test that drives the bus
will hit it.

**One v1 chord cannot be had.** v1's fields accept `ctrl+h` as delete-backwards;
in v2 `ctrl+h` is a reserved focus key the kernel consumes before any plugin sees
it. `backspace` covers it, and the reserved set is the better trade — but it is a
real difference in a text field, noted in `ui/lib/textinput.lua`.

**Two spec scenarios needed narrowing, not the implementation.** Creating with
nothing selected spawns in the home directory in v1; for a *remote* target there
is no local home to stand in and `create` needs a path on the machine that will
run the session, so the flow asks for a repository there instead. It is recorded
as a divergence in `tests/v2_parity.rs` rather than left as a scenario the
implementation quietly fails.

**The flow was correct and the session still had no terminal.** Running it found
Tier-0 gap #1 immediately: a **local** spawn leaves `backend_id` empty for the
interface to resolve by name (v1 does that when it adopts windows at restore),
and the v2 kernel never learned to — so a created session had a live agent, a
real `tb-<name>` window, and a pane saying "session has no pane yet". Worse, the
failure was keyed by session alone and so latched forever, which meant *every*
session this flow creates was unattachable for the life of the process. Fixed in
`kernel::terminal` alongside this change, because a flow that creates sessions
you cannot use is not a delivered feature: resolution by window name (throttled,
local backends only — a remote spawn records its real id, and readying a remote
backend here would put an ssh connect on the render thread) and an attach
attempted once per *(session, pane)* instead of once per session.
