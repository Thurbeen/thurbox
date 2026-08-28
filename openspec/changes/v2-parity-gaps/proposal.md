## Why

`v2-retire-v1` cannot run while v2 is behind v1, and until now the gap was
tracked only as the nine rendering divergences in `tests/v2_parity.rs`. A
surface-by-surface audit of every v1 pane against its plugin — reading both
sides and citing each — found the real number is far larger, and that several
are not cosmetic: **a session created in v2 never gets a terminal**, automations
never fire, and restore only clears a flag.

This change records that inventory so the remaining work is a list rather than a
rediscovery, and so a fix can be checked off against a named gap.

Scope note: the **code-review** surface was deliberately excluded from the audit
(`v2-code-review` owns it), so nothing here concerns `ui/plugins/75_review.lua`,
`src/kernel/diff.rs`, or their v1 counterparts.

## What Changes

Nothing, by itself — this change is the ledger. Each work package below becomes
an edit under its own commit, and the gap it closes is struck from the list.

Already closed while the audit ran (verified by tests, not by inspection):

- Session rows carry the agent's activity title and a blocked session's own
  message (`10_sessions.lua`, `kernel::terminal::AgentMeta`).
- A multi-repo session gets its own `a + b` repo group, keyed on the repo set.
- The info panel draws the System, Agent and Usage sections, the `Activity:` and
  `Signal:` rows, and every repo — behind a new `kernel::metrics` worker.
- Task detail dates a task (`created`/`updated`), via a shared `widgets.time_ago`.
- The mouse wheel works: modal step, float, SGR forwarding to a mouse-tracking
  agent, and the pane under the pointer.
- Drag-selection works at all (`?1002` was never requested, so no drag report
  ever arrived).
- `Ctrl+V` pastes (no `arboard` handle was ever constructed, so every paste
  reported an unreachable clipboard), and a terminal-native paste arrives
  bracketed instead of submitting on its first newline.
- A finished turn's filled dot goes hollow when you move off it (`seen_at` was
  read by `derive_status` but written by nobody).
- `Shift+J`/`Shift+K` drag a subtree and move a whole group past a group edge,
  and `Shift+S` sorts within each group — computed over the rendered items and
  persisted as one explicit order (`Command::Order`).
- Startup heals active extensions and auto-activates the built-in `hooks`
  extension, without which a fresh profile reports no status at all; and it arms
  the tmux automation heartbeat.
- An idle snapshot no longer re-reads five tables every 400ms (`PRAGMA
  data_version` gate), and a quiet terminal pane no longer defeats the
  demand-driven paint (output-stamp gate).

## The interface was then cut back to two panes

After this audit, the bundled interface was deliberately reduced to
`10_sessions` and `20_agent` — the session list and the agent pane. Every other
plugin was deleted, along with its slot in `layout.lua`, its entry in
`kernel::bundled::BUNDLED`, and its tests.

This does not shrink the gap; it makes the gap explicit. Most of the behavioural
gaps below now begin with "the pane does not exist", and the panes that were
already thin (search, tasks, automations) are better rebuilt against the fixed
core than patched in place. Two guardrails keep the shortfall counted rather than
forgotten:

- `tests/v2_keymap.rs::CHORDS_AWAITING_THEIR_PANE` — every v1 global chord whose
  owner is gone, asserted **unbound**. A freed chord must never be quietly
  reused, or a v1 user's muscle memory would start doing something else.
- `tests/v2_parity.rs::v2_covers_every_pane_v1_had` — the two halves, asserted
  separately: the panes that exist, and the twelve that are knowingly absent.

Re-adding a pane is therefore a mechanical checklist: the plugin file, its slot
in `layout.lua`, its entry in `BUNDLED`, its line moved from the "awaiting" list
into the covered one, and its chord moved out of `CHORDS_AWAITING_THEIR_PANE`.

Fixed at the same time, both found by using it rather than by reading it:

- **Hovering an affordance never lit it.** The terminal was asked for `?1000`
  (clicks) only, so no motion report ever arrived and the whole hover layer was
  inert. It now asks for `?1003`.
- **A plain click "selected" part of the interface.** A press that nothing else
  consumed arms a selection so the same press can start a drag; an *unextended*
  one was still painted, reversing the cell under the pointer. It is now armed
  but not drawn until it has an extent.

## Capabilities

### Modified Capabilities

- `bundled-plugins`: the bundled interface must reach v1's behaviour, not merely
  cover its panes. `tests/v2_parity.rs::v2_covers_every_pane_v1_had` answers the
  weaker question and stays; this list is the stronger one.

## Impact

`v2-retire-v1` gains a prerequisite: every Tier 0 and Tier 1 package below.
Tier 2 and Tier 3 are parity too, but a v2 missing them is usable, where a v2
that cannot attach a terminal to a session it created is not.

## Verified gaps

| # | Gap | Severity | Where the fix goes |
|---|---|---|---|
| ~~1~~ | ~~**A locally- or CLI-created session never gets a terminal.** No adopt-by-window-name anywhere, and `sync` latches `"session has no pane yet"` forever.~~ **Closed**: `Terminals::sync` resolves a paneless row by its window name (`tb-<name>`, throttled, local backends only — a remote spawn records its real id) and an attach is now attempted once per *(session, pane)* rather than once per session, so a window that appears later is picked up. Found by using the creation flow. | ~~blocker~~ | done |
| ~~2~~ | ~~Attach runs inline on the render loop.~~ **Closed**: `ensure_ready`, the history capture and `adopt` all run on a worker that enters the interface's runtime, published back over an mpsc the loop drains — so a host that is down runs out its ssh timeout without a frame being missed. The first attach on a backend is what opens its connection and the rest wait for it, the seed is passed to `adopt` rather than `None` (an adopted pane shows the conversation already in it), and the same failed attempt is retried every 20 s instead of never. | ~~blocker~~ | done |
| ~~3~~ | ~~**No `unreachable` state.**~~ **Closed**: `snapshot::with_reachability` folds the attach state into the published status — a *remote* session with no live pane reads `unreachable`, which lights the `⊘` and `status_unreachable` the theme already carried, drops the spinner and says which host is down. A local session is never unreachable: this is its machine, and a missing pane there means the agent was not launched. A live remote session whose connection dies is let go the way v1 does it (`has_exited` under `remain-on-exit`) so it lands in the same state rather than painting a frozen screen. | ~~behavior~~ | done |
| ~~4~~ | ~~**Remote status is frozen.**~~ **Closed**: `Terminals::drain_hook_events` collects every backend's queue and `SnapshotStore::apply_hook_states` writes it into the columns a local `session signal` writes — matched on backend *and* pane (ids collide across hosts), allow-listed (the value is written on a machine we do not control), de-duplicated against the raw hook state so a reconnect cannot resurrect an acknowledged `done`, and parked for 120 s when the pane has not been attached yet. | ~~behavior~~ | done |
| 5 | **Automations never fire.** No due-automation pass in the loop, no `claim_due_automation`, no `arm_automation_heartbeat`. `r` only marks the row due (`command.rs:950`), so it is observably a no-op. | blocker | `src/bin/thurbox2.rs` |
| ~~6~~ | ~~**On a fresh profile nothing reports status.**~~ **Closed**: `thurbox2`'s `main` runs `heal_active_extensions` and `ensure_builtin_hooks_extension` before it takes the terminal, exactly where v1 runs them. | ~~blocker~~ | done |
| ~~7~~ | ~~Wizard cannot **name** a session, cannot pick a **base branch**, and its repo step only lists repos of *existing* sessions.~~ **Closed** by `v2-new-session-flow`: name and branch-name steps, a fetched base-branch list, and host-scoped repository memory (`kernel::repos`) with a typed path, a browse dropdown and folder import. | ~~blocker~~ | done |
| ~~8~~ | ~~Host step is **last** with no `local` option, and one configured host forces every session onto it; `create` also stats a remote repo path locally.~~ **Closed** by `v2-new-session-flow`: the host step is first, offers the local machine, is skipped only when nothing is configured, and the local stat is skipped when a host is named. | ~~behavior~~ | done |
| ~~status bar~~ | ~~The footer is gone: no focus label, no counts, no pills.~~ **Closed** by `v2-chrome-bands` — back as a kernel-rendered band with contributed entries. | ~~visual~~ | done |
| ~~header band~~ | ~~No brand, version or theme row.~~ **Closed** by `v2-chrome-bands`. The update notice renders but nothing detects a release yet — that half stays open under the update-check gap. | ~~visual~~ | done |
| ~~9~~ | ~~Multi-repo creation unreachable: single-select repo step, no extra-repo field on `Command::Create`.~~ **Closed** by `v2-new-session-flow`: `space` multi-select with a per-repo `w`, carried as `extras` into `SpawnRequest.extra_repos`. | ~~missing~~ | done |
| ~~10~~ | ~~Wizard rows are unclickable: `on_click` does `tonumber(hit.id)` on a path.~~ **Closed** by `v2-new-session-flow`: a repository row is matched by its path, a selector row by its position. | ~~polish~~ | done |
| ~~11~~ | ~~**Restore only clears the flag.**~~ **Closed**: `session_ops::restore_session_headless` restores the row, re-attaches every worktree whose branch survives, and re-launches the agent through the restart plan — so a restored session resumes rather than landing in `failed`. `best_effort` is now read: a force-deleted row is refused without it. Remote restore stays local-only: unlike a restart, it has to recreate worktrees, which cannot be done from here. | ~~behavior~~ | done |
| ~~12~~ | ~~A soft-deleted session's pane/agent is **never reaped**.~~ **Closed**: `kernel::reaper` watches ids leave the snapshot and, once v1's 10s undo window closes, dispatches a `Reap` command — `session_ops::reap_soft_deleted` kills the window and removes the metrics file and the symlink workspace, leaving the worktrees that make the undo lossless. Coming back cancels it, which is what an undo is. | ~~behavior~~ | done |
| ~~13~~ | ~~**Fork doesn't fork.**~~ **Closed**: `SpawnRequest` grew `fork_session_id` and `inherit_worktrees`, so a fork launches in the parent's *worktree* (its cwd), on the parent's host, carrying the parent's conversation id — which is what makes the agent's `fork_args` fire instead of starting a stranger. | ~~behavior~~ | done |
| ~~14~~ | ~~**Sync** touches only `worktrees.first()` and always calls the local `git::sync_worktree`.~~ **Closed**: every worktree is synced (a multi-repo session left half-rebased spans repositories at different bases), through `sync_worktree_on` against the session's own host. A host `hosts.toml` no longer describes is refused rather than silently synced locally. | ~~behavior~~ | done |
| ~~15~~ | ~~**Restart refuses remote sessions**, via `restart_session_headless`'s "restart it from the TUI instead" — inside the TUI.~~ **Closed**: the restart plan is host-aware (`adapt_def_for_launch`, so hook configs are shipped and `{home}` resolves on the host; local path env skipped), the old pane is killed by id on its host and the new pane id persisted. Only an unresolvable host is refused. | ~~behavior~~ | done |
| ~~16~~ | ~~`Ctrl+O` passes only `row.cwd` to `open_editor`.~~ **Closed**: `SessionRow.member_dirs` carries every repository's *checkout* (not its root — the editor has to land on the branch being worked) and all of them are opened. The terminal-vs-GUI classification moved to `session::editor`, which both binaries now share, so a GUI editor is spawned detached rather than handed a tty it may hold open. | ~~behavior~~ | done |
| ~~17~~ | ~~Hard-delete confirm is one line; `session.git` is published and unused.~~ **Closed** by `v2-core-settings`: `60_confirm.lua` is back, and the session list itemises uncommitted files, unpushed commits and the worktree from `session.git` — including the case it cannot read, which is reported rather than assumed clean. | ~~behavior~~ | done |
| ~~R1~~ | ~~**A restarted session stays attached to the pane it just killed.** The local restart discards the new pane (`spawn_window` reports none) and leaves the old id on the row, while `Terminals` only let go of *remote* sessions whose reader hit EOF — so `Ctrl+R` left a frozen last frame that took no keys.~~ **Closed**: the restart clears the stale id and *tells* the interface to let go (`Terminals::forget`), which then re-resolves the new window by name. Detecting it does not work and the first attempt to was wrong: `has_exited` fires when a session's output **stream** ends, and control mode carries every pane on a backend down one connection — killing a pane leaves it open, so the session looked alive while showing a pane that no longer existed. v1 needs none of this because `Session::restart` rebinds the live object in place. | ~~blocker~~ | done |
| ~~R2~~ | ~~**Sessions do not come back when thurbox restarts.** v1's restore respawns a session whose window is gone (`respawn_stale_session`), which is how one survives a reboot or a dead tmux server; v2 only ever attached, so every session sat at "no pane yet" forever.~~ **Closed**: `Terminals::missing_agents` asks v1's restore-time question — a *surveyed* backend with no window for this session — and the loop relaunches through `restart`, once per session per run. Killing a window that is not there stopped being an error, which is what makes restart a respawn. | ~~blocker~~ | done |
| ~~R3~~ | ~~**v2 is less reactive than v1.** Nothing marked the screen dirty when an agent printed: all 29 `dirty` sites are input, resize, reload or a poll result. The output stamp was only read *inside* `draw`, where it can say a frame changed but never cause one — so a printing agent was drawn at the 250 ms floor, ~4 fps.~~ **Closed**: `Terminals::output_generation` is summed in the loop each iteration and marks dirty, which is v1's `detect_output_redraw`. The `MIN_FRAME_INTERVAL` cap only starts mattering now that output can cause a frame at all. | ~~blocker~~ | done |
| ~~R4~~ | ~~**The companion shell is forgotten on every restart.** Its tmux window (`tbsh-…`) outlives the interface, but that a session *had* one lived only in the `Session` object, which does not — so restarting thurbox or a session left the shell tab empty and the window orphaned, and the next shell key spawned a second one beside it. v1 persists `shell_backend_id` and re-adopts it at restore.~~ **Closed**: the id is persisted when the shell opens and re-adopted once the agent's own pane lands, with v1's guard — a pane that no longer exists is forgotten rather than adopted as a dead surface. | ~~behavior~~ | done |
| ~~R5~~ | ~~**Only failures were reported.** A finished command simply left the in-flight list, so a successful restart, sync or delete said nothing, and a failure said `restart: <error>` without naming which session.~~ **Closed**: `kernel::messages` names the verb and the subject on both paths, capturing the subject while the row still exists (a delete's is gone by the time it reports), and stays silent for outcomes already visible on screen. | ~~polish~~ | done |
| ~~R6~~ | ~~**The shell opens in the wrong directory.** `open_shell` passed no cwd, so `ensure_shell_pane` fell back to `SessionInfo.cwd` — and v2 *adopts* rather than restoring, so `Session::adopt` builds a fresh `SessionInfo` whose cwd is always `None`. Every shell therefore inherited the multiplexer's directory, which is wherever thurbox was started, not the session's repository.~~ **Closed**: the launch directory is resolved from the snapshot row (`Terminals::launch_cwd`, v1's `session_process_cwd_existing`) — the repository for one, the symlink workspace the agent is actually running in for several — and falls back to the recorded cwd when the workspace cannot be named. | ~~behavior~~ | done |
| ~~18~~ | ~~`DeletedRow` is `{id, name, partial}` — restore rows drop agent, deletion age and `[wt]`.~~ **Closed** with the restore list itself (`ui/plugins/80_restore.lua`, v1's `Ctrl+U`): the row carries `agent`, `deleted_at` (epoch millis, measured against `taken_at_ms` — a plugin has no clock) and a worktree count, so a row reads `name (agent) 1h ago [wt]` as v1's did. A force-deleted one is tagged on the row *and* asks through `store.confirm`, rather than needing v1's bespoke `ConfirmRestore`. | ~~visual~~ | done |
| ~~19~~ | ~~**`Enter` on a session row does nothing.**~~ **Closed**: `enter` is declared (so help lists it and it can be rebound) and focuses the agent pane, which is what opening a session means here. | ~~missing~~ | done |
| ~~20~~ | ~~The list **overwrites `store.selected` every render**, so no other pane can steer it.~~ **Closed**: the list remembers what it published, so a value it did not write is read as a request and followed — which is what lets a search result or a task jump to its session. | ~~behavior~~ | done |
| ~~21~~ | ~~Search `Enter` doesn't jump.~~ **Closed**: the strip is back as `ui/plugins/65_search.lua` and `Enter` selects the result and focuses the session's terminal, which is where v1's Enter lands you. `panels.show` was added so a jump can open a closed column rather than focusing nothing. Sessions is the only scope with a pane today; the result shape carries the pane it belongs to, so a returning pane is a scope added and nothing else changed. | ~~behavior~~ | done |
| ~~22~~ | ~~No **live preview** and no cancel-restore snapshot in search.~~ **Closed**: moving the selection — and typing, which moves the result out from under it — writes `store.selected`, so the list's cursor follows while focus stays in the strip. Opening captures the prior selection and panel state; `esc` puts them back, since previewing has already moved a cursor by then and closing without restoring would make cancelling a way to change the selection by accident. | ~~behavior~~ | done |
| ~~23~~ | ~~Search is a **centered 60×55 float** covering the panes it highlights.~~ **Closed**: a `search` slot in `ui/layout.lua`, placed above the bands and shrinking the content the way a side column does. Not a float on purpose — a float would cover the rows it is pointing at, which is the whole argument for highlighting in place. | ~~visual~~ | done |
| ~~24~~ | ~~Search matches **plain substring**, not v1's subsequence fuzzy.~~ **Closed**: `ui/lib/fuzzy.lua` ports `src/fuzzy.rs` and returns the matched positions, so `fb` finds `fix-branch` and every matched character is lit rather than one contiguous run. Shared by the strip and by the session list, which must agree on which characters hit or the highlight lands on the wrong letters. | ~~behavior~~ | done |
| ~~25~~ | ~~No **buffer-content** search.~~ **Closed**: a session is found by what its terminal is showing, with the matching line as the snippet. The read is *asked for* rather than published — the pane leaves its query under `want_content` and the kernel serves `thurbox.content` only while it is asking, so no interface pays for every agent's screen on every frame. Debounced on `ctx.elapsed` (v1's 150 ms), substring rather than subsequence (fuzzy over a whole screen matches everything), and skipped for a session whose metadata already matched. | ~~missing~~ | done |
| 26 | No **Files** scope in search and no reveal-path hook in the file pane. | missing | `ui/plugins/65_search.lua`, `ui/plugins/55_files.lua` |
| ~~27~~ | ~~Strip shows one total count.~~ **Closed**: `[n/total]` beside the query, a per-scope summary line, a header per scope, `widgets.list`'s `▸` on the selected result, and a snippet naming the field when a session matched on something other than its name. | ~~visual~~ | done |
| ~~28~~ | ~~Search query has no caret editing.~~ **Half closed**: the query is a `lib.textinput`, so it has v1's caret and readline chords. `Ctrl+P`/`Ctrl+N` are deliberately **not** taken — a plugin-scoped chord does not outrank a global one in this kernel, so declaring them would take `ctrl+n` from new-session everywhere rather than only inside the strip. Recorded in `KNOWN_DIVERGENCES`; both keys are declared and rebindable. | ~~polish~~ | done |
| ~~29~~ | ~~Session-list rows are not dimmed while searching, and the whole row is accented rather than the matched run.~~ **Closed**: the list reads the query off `store` and answers for its own rows — matched characters accented, a row nothing matched dimmed rather than hidden, so the list never jumps around under a cursor you are still moving. | ~~polish~~ | done |
| 30 | **No task editor at all**: `Command::Task` has no `description`, no `n`/`e` keys, no editor plugin — while `35_tasks.lua:338` and `36_task_detail.lua:158-170` draw `e edit  r run  n new`. | missing | `src/kernel/command.rs`, new `ui/plugins/37_task_editor.lua` |
| 31 | `o` (open related session) is drawn as a hint and bound nowhere (`grep 'key = "o"' ui/` → nothing). | missing | `ui/plugins/35_tasks.lua` |
| 32 | Dispatch picker offers only `store.selected`, not one Send row per running session, and shows no task title. | behavior | `ui/plugins/35_tasks.lua` |
| 33 | "Create a session for it" **guesses** the repo (`task_repo` = most recent) instead of running the wizard; fails outright with no active session. | behavior | `ui/plugins/35_tasks.lua`, `src/kernel/command.rs` |
| 34 | Dispatch records no task→session link, so `⇄` and the detail `session` row stay empty. | behavior | `src/kernel/command.rs`, `ui/lib/tasks.lua` |
| 35 | **No automation authoring**: `Command::Automation` is `{id, enabled, run_now, delete}` only; `46_automation_detail.lua` declares no keys; empty state says "Ctrl+N to add", which opens the new-*session* wizard. | missing | `src/kernel/command.rs`, `ui/plugins/46_automation_detail.lua` |
| 36 | Run history is not navigable: no cursor (window pinned to newest), no `Enter`→session, and `RunRow` lacks `related_session_id`. | missing | `src/kernel/snapshot.rs`, `ui/plugins/46_automation_detail.lua` |
| 37 | `next_run_at` is not in the snapshot → the info panel prints the cron string where v1 shows a countdown, and the pane's `when` field reads `—` always. | behavior | `src/kernel/snapshot.rs`, `ui/plugins/25_info.lua` |
| 38 | `j`/`k` does not cross between the session list and the automations pane — both wrap modulo their own list, so the pane is reachable only via `Ctrl+P` or a click. | behavior | `ui/plugins/10_sessions.lua`, `ui/plugins/45_automations.lua` |
| 39 | No **companion** rule: focusing the automations pane leaves the centre on the agent, and after one visit to the detail the agent never comes back. | behavior | `src/kernel/host.rs`, `src/bin/thurbox2.rs` |
| ~~40~~ | ~~**`[features]` is unhonoured**.~~ **Closed** by `v2-core-settings`: `settings::init` runs before anything reads a flag, the flags are published to plugins, and each one gates the surface it names. | ~~behavior~~ | done |
| ~~41~~ | ~~The settings modal renders **only plugin-declared settings**.~~ **Closed** by `v2-core-settings`: a kernel-owned group carries the `settings.toml` fields v2 honours beside the plugin ones, with v1's draft/`Ctrl+S` and the live-vs-`⟳` split. | ~~missing~~ | done |
| 42 | **Scrollback is wrong in five ways**: fixed 10-line pages (v1: half height), offset never clamped so `[N↑]` lies and PageDown needs many presses, scrollbar scaled against a `scroll_max` high-water mark, cursor still painted mid-history, shell tab doesn't scroll, and typing doesn't snap to bottom. | behavior | `src/kernel/terminal.rs`, `ui/plugins/20_agent.lua` |
| 43 | ~~Terminal scroll chords are **undeclared**~~ **Half closed**: `pageup`/`pagedown` are declared as `terminal.scroll_up`/`terminal.scroll_down` (plugin-scoped), so help lists them and they can be rebound; the action still declines on the shell tab so the pty keeps the key there. Still open: no `Shift+Up/Down`, and a bare `PageUp` on the agent tab is consumed rather than also reaching the agent. | behavior | `ui/plugins/20_agent.lua` |
| 44 | Scrollbars are painted with no `role`/`id` anywhere — the terminal, file tree and task description bars are all non-interactive. | missing | `ui/plugins/20_agent.lua`, `ui/plugins/55_files.lua`, `ui/plugins/36_task_detail.lua` |
| 45 | Only the **painted** pane is resized; background sessions keep the whole-screen size they were adopted at, so switching to one reflows. | behavior | `src/kernel/terminal.rs`, `src/bin/thurbox2.rs` |
| 46 | File viewer has **one root per session** (`host.rs` `roots.insert(id, cwd)`), so a multi-repo session's other worktrees and attached dirs are unreachable — `files::resolve` refuses them. | missing | `src/kernel/host.rs`, `src/kernel/snapshot.rs`, `ui/plugins/55_files.lua` |
| 47 | `/` in the tree never expands collapsed directories, so deep matches are invisible and the `(c/t)` count only counts visible rows; and it never moves the cursor (no jump-to-first, no `n`/`N`). | behavior | `ui/plugins/55_files.lua` |
| 48 | `Enter` on a file opens an in-pane viewer with **unclamped** scroll instead of your editor; `Command::Editor` has no `path` form. | behavior | `src/kernel/command.rs`, `ui/plugins/55_files.lua` |
| 49 | The tree calls `files.list` for the root and every expanded directory **on every paint**, and re-reads + re-splits the previewed file every paint. No memoisation. | perf | `ui/plugins/55_files.lua` |
| 50 | Arrow keys and `l` are unbound in the tree (only `j`/`k`/`enter`/`h`/`/`). | behavior | `ui/plugins/55_files.lua` |
| 51 | `nerd_font_enabled` is explicitly destructured away (`theme.rs:196`), so a nerd-font theme still shows `▾`/`▸`. | visual | `src/kernel/theme.rs`, `ui/plugins/55_files.lua` |
| 52 | `info.open` **focuses** the info pane, which declares no `on_key` and no session input — so while it holds focus keystrokes reach neither the agent nor the list. v1's `ToggleInfoPanel` never moves focus. | behavior | `ui/plugins/25_info.lua` |
| 53 | `Hooks: degraded — <reason>` row missing (`hook_wiring` appears nowhere in v2), so a session whose hooks were stripped reads as an idle agent. | missing | `src/kernel/command.rs`, `src/kernel/host.rs`, `ui/plugins/25_info.lua` |
| 54 | `Disk  N MB (thurbox dir)` row missing from the System section. | missing | `src/kernel/metrics.rs`, `ui/plugins/25_info.lua` |
| 55 | Notification settings ignored: `Notifier::new(enabled, suppress_active)` only — no `also_on_waiting` (Working→Done edge), no `min_interval_secs` dedup, `sound: true` hardcoded, and the body is a synthetic `"{agent} is waiting for input"` even though `AgentMeta::notification` is published. | behavior | `src/kernel/notify.rs`, `src/bin/thurbox2.rs` |
| 56 | **Click-to-focus is dead**: `pending_focus_session_id` is never drained, so a notification click *and* `thurbox-cli session focus` both do nothing while v2 accumulates unread rows. | missing | `src/kernel/snapshot.rs`, `src/bin/thurbox2.rs` |
| 57 | **No external config live-reload** (the only watcher is scoped to `ui/`): `agents.toml`, `hosts.toml`, `keybindings.json`, `settings.toml`, `themes.toml` are all read once. `Themes::refresh` exists with no caller, so a theme picked in another instance is never adopted. | missing | new `src/kernel/config_reload.rs`, `src/bin/thurbox2.rs` |
| 58 | No startup update check and no `⬆ vX.Y.Z available` header badge. | missing | `src/bin/thurbox2.rs`, `ui/plugins/05_header.lua` |
| 59 | No readline line editing in any of v2's three text fields (search query, wizard branch, tree search) — backspace-plus-append only; the `input` node kind is unused by every bundled plugin. | behavior | new `ui/lib/lineedit.lua`, three plugins |
| ~~60~~ | ~~`to_press` drops `KeyModifiers::SUPER` … so Cmd is unbindable.~~ **Closed** (issue #1024): the kitty `DISAMBIGUATE` push landed with the chrome, `KeyPress` carries `cmd`, and `canonical_chord` emits it in `normalise_chord`'s order — so `cmd+…` resolves, and `Cmd+J` no longer fires the plain `j` binding. macOS builds ship `Cmd+C`/`Cmd+V` as defaults. | ~~missing~~ | done |
| 61 | `links()` runs `extract_screen_rows` + `detect_urls` with **no emptiness gate**, for **every** session, on every republish (before every paint and every key/mouse/paste) — locking each parser against its reader thread. | perf | `src/bin/thurbox2.rs`, `src/kernel/terminal.rs` |
| 62 | Every floating plugin is rendered **full-screen each frame** just to read `rendered.float` (~1.3 ms, five of them); the session list still spends 4 nodes/row; `build_model` (grouping + two sorts + nesting) is recomputed every paint. | perf | `src/bin/thurbox2.rs`, `ui/plugins/10_sessions.lua` |
| 63 | Perf HUD is six counters in hardcoded `Color::Yellow`; no frame/tick histogram, no slow-op ring, no `THURBOX_PERF_LOG`, no snapshot for `thurbox-cli perf`. | perf | `src/kernel/perf.rs`, `src/bin/thurbox2.rs` |
| 64 | `?1003` is deliberately refused, so `MouseEventKind::Moved` never arrives and **all hover styling is inert** — `ui/lib/hover.lua` and the chip/pill hover work from commits 492e912/4fdb30e can never light. | visual | `src/bin/thurbox2.rs` (`enable_mouse_clicks`) |
| 65 | Help's `RESERVED_ROWS` still advertises `tab / shift+tab → Focus next / previous pane`; Tab was deliberately given back to the agent in ad359a9. `docs/PLUGINS.md:272` repeats it. | polish | `src/kernel/modals/help.rs`, `docs/PLUGINS.md` |
| 66 | ~~`Ctrl+C`/`Ctrl+V` are matched literally ahead of the registry (unrebindable, listed under "Fixed")~~ **Half closed**: both are declarations owned by the kernel (`kernel::clipboard`), listed in help, offered by the palette and rebindable; they resolve ahead of a float's grab so they still work from any pane, and copy declines with no selection so `Ctrl+C` still interrupts. Still open: `Ctrl+C` with no selection copies nothing (v1 copies the status message); toasts carry no level, so `INFO`/`✓ SYNC`/`ERROR` badges are gone. | polish | `src/kernel/clipboard.rs`, `src/coordinator/input.rs` |

## Work packages

Ordered by impact. Each is one coherent edit; the parenthetical is every file it touches.

**Tier 0 — v2's core loop is broken without these**

1. **Adopt a pane by window name.** In `sync`, when `backend_id` is `None`, call `backend.discover()` (cached per backend), match `DiscoveredSession` on `row.name`, adopt, write the resolved id back. Replace the permanent `failed` entry with a retry deadline. Fixes #1. (`src/kernel/terminal.rs`)
2. ~~**Move attach onto a worker + retry + placeholder.**~~ **Done.** `sync` notices new and removed sessions; `ensure_ready` + `capture_history` + `adopt` run on a worker (entering the interface's runtime, since adopt wires tokio tasks) and come back over an mpsc. One attach per backend opens its connection and the others wait; the seed is passed; the same failure is retried after 20 s. Fixes #2.
3. ~~**`unreachable` status.**~~ **Done.** `snapshot::with_reachability` at publish time, plus `drop_lost_remotes` so a connection that dies mid-session reaches the same state. Fixes #3.
4. ~~**Drain remote hook events.**~~ **Done.** `Terminals::drain_hook_events()` into `SnapshotStore::apply_hook_states`, matched on (backend, pane id), allow-listed, de-duplicated against the raw `hook_state` the row now carries, with v1's pending-event TTL. Fixes #4.
5. **Startup parity.** Before `Terminals::new()`: `heal_active_extensions`, `ensure_builtin_hooks_extension`, then construct `Terminals` so it reads the patched `agents.toml`; and `arm_automation_heartbeat()` gated on `features.automations`. Fixes #6 and the durable half of #5. (`src/bin/thurbox2.rs`)
6. **In-TUI automation scheduler.** A ~1 s claim-then-fire step on the command worker (`claim_due_automation`), polled like `commands`/`diffs`. Fixes the rest of #5. (`src/kernel/command.rs`, `src/bin/thurbox2.rs`)
7. **Restore actually restores.** After `db.restore_session`, lift `recreate_worktrees` into `session_ops`, respawn through the spawn machinery with `session_id`/`resume_session_id`, preserve `backend_type`, honour `best_effort`. Fixes #11. (`src/kernel/command.rs`, `src/session_ops/`)
8. **Soft-delete reaper.** Track ids that left the snapshot with a timestamp; after the undo window kill the pane, remove the metrics file and `workspace::remove_workspace`. Fixes #12. (`src/bin/thurbox2.rs`)

**Tier 1 — creating and acting on sessions**

9. ~~**Wizard shape, Lua-only.**~~ **Done** in `v2-new-session-flow` — host-first with a `local` option, name and base steps, rows clickable by identity. Fixed #7 (name), #8 (host), #10.
10. ~~**Real repo source.**~~ **Done** in `v2-new-session-flow` — `repo_bookmarks` published through `kernel::repos` (with folder scans), `Command::Bookmark { host, path, edit }`, and a directory listing served per (host, dir); the repo step has the typed path, the `/` filter and `d`. Fixed the rest of #7.
11. ~~**Branch list + base step.**~~ **Done** in `v2-new-session-flow` — a worker fetching and listing, ordered as v1 orders it, published as `thurbox.branches`; `base` travels on `create` and the local stat is skipped for a host.
12. ~~**Multi-repo create.**~~ **Done** in `v2-new-session-flow` — `extras` on `Command::Create` into `SpawnRequest.extra_repos`, `space`/`w` and v1's `[x]`/`[wt]`. Fixed #9.
13. ~~**Fork/sync/restart correctness.**~~ **Done.** `fork_session_id` + `inherit_worktrees` + `host` on `SpawnRequest` threaded into `SessionConfig` so `fork_args` fire, launching in the parent's worktree; `sync` loops all worktrees via `sync_worktree_on(host)`; the restart plan is host-aware and persists the new pane id. `session_ops::resolve_host` is the one place that says which machine a session lives on, so an unreachable host is refused rather than acted on locally. Fixes #13, #14, #15.
14. **Ctrl+O + editor path form.** ~~Publish member dirs on `SessionRow`; pass all of them to `open_editor` and classify terminal-vs-GUI.~~ **Done** — `member_dirs` on the row, `session::editor` shared by both binaries. Still open: `path: Option<String>` on `Command::Editor`, resolved through `kernel::files::resolve`, which is what #48 needs. Fixes #16.
15. ~~**Confirm + restore-row detail.**~~ **Done.** The confirmation half landed in `v2-core-settings`; the restore half is the pane it was waiting for — `ui/plugins/80_restore.lua` is v1's `Ctrl+U` list (a float, passthrough chord, `Enter` restores, a force-deleted row asks first through the shared confirmation), and `DeletedRow` gained `agent`/`deleted_at`/`worktrees` to fill its rows. Fixes #17, #18.

**Tier 2 — navigation and search**

16. **List honours an external selection + `Enter`.** Treat a `store.selected` that differs from what this plugin last published as a selection request (reuse the `state.follow` lookup) instead of overwriting it; add `{ key = "enter", action = "sessions.open" }` → `command("focus", { text = "agent" })`. Fixes #19, #20, and the session half of #21. (`ui/plugins/10_sessions.lua`)
17. ~~**Search jump + preview + shared cursors.**~~ **Done.** `panels.show(name)`; a `preview(result)` called from next/previous *and* on every query edit; the prior selection and panel state captured in `open` and restored in `close`. No `ui/lib/automations.lua` — the automations pane does not exist yet, so there is no cursor to share. Fixes #21, #22.
18. ~~**Search becomes a strip.**~~ **Done.** A `search` slot above the bands, present when `panels.shown("search")`. It never floated in the end — the pane was rebuilt from nothing, as a strip. Fixes #23.
19. ~~**Fuzzy + strip chrome.**~~ **Done.** `ui/lib/fuzzy.lua` returning matched positions, shared by the strip and the session list; per-scope counts, `[n/total]`, group headers, `▸`, snippet rows; the caret via `lib.textinput`. `Ctrl+P`/`Ctrl+N` deliberately not taken (see #28). Also fixes #29 — the list dims what did not match. Fixes #24, #27, #28.
20. ~~**Content search capability.**~~ **Done**, but not as proposed: `terminal.text(session)` would have granted every plugin an unconditional read of every agent's screen, permanently. It is a *want* instead (`kernel::terminal::WANT_CONTENT`), served only while the pane asks — narrower, and the pattern `kernel::repos` already established. Capped at 500 lines (v1's number), debounced, snippet from the matching line. Fixes #25.
21. **Files scope + reveal.** Bounded `files.list` walk as a fourth kind; `store.files_reveal` consumed by the tree (expand ancestors, move cursor, clear). Fixes #26. (`ui/plugins/65_search.lua`, `ui/plugins/55_files.lua`)
22. **Dim the misses in the session list.** Read `store.search_query` in the row builder and apply the shared base/highlight pair with underline, as tasks/automations already do. Fixes #29. (`ui/plugins/10_sessions.lua`)

**Tier 3 — the read-only panes v1 authors in**

23. **Task editor.** `description` on `Command::Task` (parsed + written on the update path) and a new centre-slot editor plugin with title/multi-line description/status, `Enter`=newline, `Ctrl+S`=save; declare `n`/`e` in the tasks pane. Fixes #30. (`src/kernel/command.rs`, new `ui/plugins/37_task_editor.lua`, `ui/plugins/35_tasks.lua`)
24. **Task dispatch parity.** One Send row per running session + a title header; `o` → open related; hand "create a session" to the wizard via `store.pending_task` and dispatch on its success; persist the Send target so `⇄` and the detail row light. Fixes #31, #32, #33, #34. (`ui/plugins/35_tasks.lua`, `src/kernel/command.rs`, `ui/lib/tasks.lua`)
25. **Automation authoring.** Extend `Command::Automation` with the full payload and `id: Option<i64>` (create vs update), validating the cron on the worker; add an edit mode with `input` fields to the detail pane and `n`/`e` to the pane. At minimum, fix the empty-state hint that points at `Ctrl+N`. Fixes #35. (`src/kernel/command.rs`, `ui/plugins/46_automation_detail.lua`, `ui/plugins/45_automations.lua`)
26. **Run history navigation.** `related_session` on `RunRow`, published per run; `state.run_cursor` + `j`/`k`/`r`/`enter`, `▸` + selection background + the focused hint row. Fixes #36. (`src/kernel/snapshot.rs`, `src/kernel/host.rs`, `ui/plugins/46_automation_detail.lua`)
27. **`next_run_at`.** Add to `AutomationRow`, publish it, and use `format_countdown` (already present in `25_info.lua`) in both the info section and the pane's `when` field. Fixes #37. (`src/kernel/snapshot.rs`, `src/kernel/host.rs`, `ui/plugins/25_info.lua`, `ui/plugins/45_automations.lua`)
28. **One continuous left column.** `j` past the last session / `k` above the first hands focus to the automations pane with an entry hint in `store`, and the pane's ends hand it back — guarded on the pane actually being placed. Fixes #38. (`ui/plugins/10_sessions.lua`, `ui/plugins/45_automations.lua`)
29. **`companion` declaration.** Read an optional `companion` onto the plugin record; in switch-slot selection prefer the focused plugin's companion, else the slot's lowest-order member rather than the sticky selection. Declare `automations → automation_detail` and `tasks → task_detail`. Fixes #39. (`src/kernel/host.rs`, `src/bin/thurbox2.rs`, two plugins)

**Tier 4 — settings, terminal fidelity, files**

30. **Publish and honour `[features]`.** `thurbox.features` from `settings::global()`; `panels.shown` returns false for a disabled panel; the owning `*.open` actions become status hints; footer drops the pill. Fixes #40. (`src/kernel/host.rs`, `ui/lib/panels.lua`, `ui/plugins/{35_tasks,45_automations,55_files,25_info,90_footer}.lua`)
31. **`settings.toml` in the settings modal.** Seed the declaration list with a kernel-owned group built from `session::settings::Settings`; route those writes through `agent::settings_config::save_settings`; add the draft/`Ctrl+S`/`⟳` split using `Settings::restart_only_differs`. Fixes #41. (`src/kernel/registry.rs`, `src/kernel/modals/settings.rs`)
32. **Scrollback truth, kernel side.** `Terminals::scrollback(session) -> (offset, total)` probing with `set_scrollback(usize::MAX)`; hide the cursor when `scroll > 0`; pass `scroll` through the `#shell` branch. Fixes half of #42. (`src/kernel/terminal.rs`, `src/kernel/host.rs`)
33. **Scrollback + chords, plugin side.** Declare `shift+up`/`shift+down`/`shift+pageup`+`alt+pageup`/`shift+pagedown`+`alt+pagedown` in `group = "Terminal"`; page by `ctx.height // 2`; delete the bare `pageup`/`pagedown` branch so it reaches the pty; clamp against the published total and title/scale from it, dropping `scroll_max`. Fixes the rest of #42 and #43. (`ui/plugins/20_agent.lua`)
34. **Snap to bottom on a keystroke.** Zero the offset when a key is forwarded to the pty. Fixes the last part of #42. (`src/bin/thurbox2.rs` or `ui/plugins/20_agent.lua`)
35. **Interactive scrollbars.** Give bar rows a `role`/`class` and map `hit.y` to a position in each owner's `on_click`; lift `scrollbar_cells` into `ui/lib/widgets.lua` and give the task description a track. Fixes #44 and the last task item. (`ui/lib/widgets.lua`, `20_agent.lua`, `55_files.lua`, `36_task_detail.lua`)
36. **Resize every session.** `Terminals::resize_all(rows, cols)` called on `Event::Resize` and after layout resolution, keeping the per-paint resize as the correction. Fixes #45. (`src/kernel/terminal.rs`, `src/bin/thurbox2.rs`)
37. **Multi-root file viewer.** `member_dirs` on `SessionRow`; `Roots` keyed by `(session, root_index)`; `files.list`/`files.read` take an optional index; one root row per member. Fixes #46. (`src/kernel/snapshot.rs`, `src/kernel/host.rs`, `ui/plugins/55_files.lua`)
38. **Tree search + keys.** `expand_for_search(query)` with v1's 5000-node/6-level budget; jump-to-first-match on every edit; `n`/`N` + `enter`/`up`/`down`/`ctrl+n`/`ctrl+p` stepping; alternate chords `down`/`up`/`left`/`right`/`l`. Fixes #47, #50. (`ui/plugins/55_files.lua`)
39. **Open files in the editor.** `files.open` on a file sends `command("editor", { session, path })` (needs WP14); clamp the in-pane viewer's scroll. Fixes #48. (`ui/plugins/55_files.lua`)
40. **Memoise the tree.** `state.listing_cache[path]` invalidated in `set_expanded` and on session change; cache the previewed file's split lines keyed on `(session, path)`. Fixes #49. (`ui/plugins/55_files.lua`)
41. **Publish `nerd_font`.** As a non-colour field beside the roles, exposed in `ui/lib/theme.lua`, used by `row_marker`. Fixes #51. (`src/kernel/theme.rs`, `ui/lib/theme.lua`, `ui/plugins/55_files.lua`)

**Tier 5 — info panel, cross-cutting, perf, polish**

42. **Info panel is toggle-only.** Drop the `command("focus")` from `info.open` and set `focusable = false`; drop the footer's `info` focus entry; correct the header comment. Fixes #52. (`ui/plugins/25_info.lua`, `ui/plugins/90_footer.lua`)
43. **Hooks-degraded + Disk rows.** Capture the degradation note the spawn path only `warn!`s, cache it per session, publish as `session.hook_wiring`; add a data-dir size sample to the metrics worker and a `Disk` row. Fixes #53, #54. (`src/kernel/command.rs`, `src/kernel/metrics.rs`, `src/kernel/host.rs`, `ui/plugins/25_info.lua`)
44. **Notifications parity.** Take the whole `NotificationSettings`; add the Working→Done arm, a per-session throttle map, `sound` from config, and the OSC body from `Terminals::meta()` truncated to 200 chars. Fixes #55. (`src/kernel/notify.rs`, `src/bin/thurbox2.rs`)
45. **Click-to-focus drain.** `SnapshotStore::take_pending_focus()` called once per iteration, applied by setting the shared selection key and focusing the agent. Fixes #56. (`src/kernel/snapshot.rs`, `src/bin/thurbox2.rs`)
46. **Config live-reload + theme sync.** A config-dir watcher/mtime poller rebuilding the agent+host registries, calling `Themes::refresh`, re-reading live flags and toasting; plus a `metadata.active_theme` compare on each snapshot refresh. Fixes #57. (new `src/kernel/config_reload.rs`, `src/bin/thurbox2.rs`)
47. **Update check + badge.** Lift `spawn_auto_update` into `agent::version_check` for both binaries, drain it in the loop, publish `thurbox.update_latest`, render the `⬆` span. Fixes #58. (`src/bin/thurbox2.rs`, `src/kernel/host.rs`, `ui/plugins/05_header.lua`)
48. **Readline line editing.** `ui/lib/lineedit.lua` implementing v1's `apply_ctrl_line_edit` set over `{text, cursor}` and consuming every remaining `ctrl+letter`; routed from the three text fields, which emit real `input` nodes so the caret is kernel-painted. Fixes #59. (new `ui/lib/lineedit.lua`, `65_search.lua`, `70_new_session.lua`, `55_files.lua`)
49. **Cmd chords.** `cmd: bool` on `KeyPress`, set from `SUPER`, emitted by `canonical_chord`; push/pop `DISAMBIGUATE_ESCAPE_CODES` in setup and the panic hook; declare the four macOS alternates. Fixes #60. (`src/kernel/host.rs`, `src/bin/thurbox2.rs`, `src/kernel/registry.rs`, `ui/plugins/10_sessions.lua`)
50. **Link scan gate.** Scan only the focused/selected session in `republish`; add the emptiness gate `hyperlink_paints` already has plus a per-session cache keyed on the output generation. Fixes #61. (`src/bin/thurbox2.rs`, `src/kernel/terminal.rs`)
51. **The two named perf levers.** Skip rendering a closed float (a cheap open-predicate the host answers, or re-probe only when input/commands set `dirty`); memoise `build_model` on a signature of (id, repos, display_order, parent). Fixes #62; the borders-as-columns half stays with `v2-chrome-in-the-kernel` task 3.1. (`src/bin/thurbox2.rs`, `ui/plugins/10_sessions.lua`)
52. **Perf observability.** A duration histogram + slow-op ring in `perf.rs` fed around `terminal.draw`/`republish`/`draw_plugin` behind a cached `THURBOX_PERF_LOG` bool; grow and theme the HUD; log a `startup` line and `perf_window` lines; publish the JSON `thurbox-cli perf` reads. Fixes #63. (`src/kernel/perf.rs`, `src/bin/thurbox2.rs`)
53. **Make hover reachable.** Either send `?1003h` (accepting the motion flood, as v1's `EnableMouseCapture` does) or delete `ui/lib/hover.lua` and the chip/pill hover styling as unreachable — currently the code exists and can never fire. Fixes #64. (`src/bin/thurbox2.rs`)
54. **Two docs/chrome corrections.** Delete the `tab / shift+tab` reserved row and the matching `docs/PLUGINS.md` line; declare `clipboard.copy`/`clipboard.paste` as kernel bindings resolved through the registry, add the copy-the-status-message fallback, and carry a level on toasts so the `INFO`/`✓ SYNC`/`ERROR` badge returns. Fixes #65, #66. (`src/kernel/modals/help.rs`, `docs/PLUGINS.md`, `src/kernel/modals/mod.rs`, `src/bin/thurbox2.rs`)

## Rejected (not real gaps)

- **`done` never acknowledged / `seen_at` never written** — `SnapshotStore::acknowledge` (`snapshot.rs:308`) writes it, called from `thurbox2.rs:753` on focus-leave, with the write-through re-derive.
- **`Shift+J`/`K` is a flat single-row swap** — the plugin now ports `move_in_order` in full (`block_end`/`root_ranges`/`child_ranges`, `10_sessions.lua:651-765`) and sends `command("order", { list })`; `Command::Order` persists the rendered order. The `reorder` arm the auditor read is a legacy path the pane no longer uses.
- **`Shift+S` sort absent** — declared at `10_sessions.lua:857`, implemented as `sorted_within_groups`.
- **Multi-repo sessions get no `a + b` group** — `SessionRow.repos` is published (`host.rs:612`) and the group key/label are built from it (`10_sessions.lua:203-240`).
- **Row shows only the word "Blocked"** — `agent_status_text` (`10_sessions.lua:414-431`) returns `session.notification or "Blocked"`, else the trimmed activity; both are published from `AgentMeta`.
- **Mouse wheel dropped kernel-wide** (claimed three times: session list, agent pane, file tree) — `MouseEventKind::ScrollUp/ScrollDown` → `on_scroll` (`thurbox2.rs:1527`), which offers the tick to modals, floats, `Terminals::forward_wheel` (real SGR forwarding, `terminal.rs:292`), then the pane under the pointer.
- **Drag-selection can never fire (`?1002` missing)** — `enable_mouse_clicks` sends `\x1b[?1000h\x1b[?1002h\x1b[?1006h`. Only the hover half survives (gap #64).
- **No `arboard` handle; Ctrl+V dead** — `clipboard: arboard::Clipboard::new().ok()` on `App` (`thurbox2.rs:123`), passed at all four call sites, with `PASTE_UNAVAILABLE_HINT`.
- **Bracketed paste never enabled** — `EnableBracketedPaste` at `thurbox2.rs:185`, `DisableBracketedPaste` in `restore_terminal`, `Event::Paste` arm at `:821`, wrapped at `:1974`.
- **Visible terminal repaints at 60 fps forever** — the surface exception is now gated on `Terminals::output_stamp` (`thurbox2.rs:1190-1203`), the v2 spelling of `detect_output_redraw`.
- **OSC title / OSC 9-777 captured but never surfaced** — `Terminals::meta()` is generation-gated, cloned each `republish`, and published per session; the list and info panel both render it. (The notify body still ignores it → folded into gap #55.)
- **Agent metrics section missing** — `src/kernel/metrics.rs` (new, untracked) samples it; `25_info.lua:291` renders cost/time/tokens/context gauge/lines/cache.
- **Usage / rate-limit section missing** — `metrics.usage` keyed on `(agent, host)`; `usage_lines` at `25_info.lua:350` with the reset countdown.
- **System / session gauges missing** — `resource_lines` + `system_lines` (`25_info.lua:263,382`) with `gauge_lines`, `format_bytes_pair`. Only the `Disk` row is genuinely absent (gap #54).
- **`Activity:` / `Signal:` rows missing** — `25_info.lua:129,139`.
- **Repos section lists only the primary repo** — the continuation loop is at `25_info.lua:161`.
- **Snapshot refresh unconditional, no `data_version` gate** (claimed twice) — `rows_are_current()` (`snapshot.rs:324-335`) reads `database.data_version()` and short-circuits the rebuild. The N+1 `list_automation_runs` per automation remains, but now only on a change — too small to package alone.

Two further claims were **narrowed rather than rejected**: the "mouse capture" gap survives only as dead hover styling (#64), and the "scrollbar drawn but not clickable" claim is true for three panes, not one, so it is packaged once (#44).