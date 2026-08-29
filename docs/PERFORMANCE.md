# Performance

> **Two eras in one file.** ADR-P1 through ADR-P12 were measured against v1's Rust
> interface and cite `src/app/*` / `src/ui/*`, both deleted when the plugin kernel
> took the binary name (ADR-23). They are kept because the *findings* are what the
> kernel was built against, and several are load-bearing in it today — the
> demand-driven loop, the 250 ms floor, output-driven dirty marking, and reading the
> hook rows only when `PRAGMA data_version` moves all carried across, in
> `src/main.rs` and `src/kernel/` rather than `src/app/`.
>
> **ADR-P13 below is the current shape**, and the one to read first if you are
> changing the loop.

How thurbox stays responsive and light, and how to measure it. The focus areas
are **input latency**, **runtime CPU / render cost**, **startup time**, and
**memory / binary size**. Decisions below follow the mini-ADR format
(**Choice**, **Why**, **Rejected alternatives**), matching
[`ARCHITECTURE.md`](ARCHITECTURE.md).

The guiding principle is **measure first**: every optimization here is backed by
a deterministic counter or a concrete code fact, and is proven by a test that
fails if the optimization regresses.

---

## ADR-P1: Demand-driven rendering (redraw throttling)

**Choice**: The render loop (`run_loop` in `src/main.rs`) paints a frame only
when the UI is *dirty* or a forced-redraw floor elapsed — not on every loop
iteration. State drives the paint:

- **Input** marks the UI dirty: `App::update` calls `App::request_redraw`, so a
  keypress paints on the very next iteration (latency unchanged).
- **Agent output** marks the UI dirty: `App::detect_output_redraw` sums each
  session's monotonic `last_output_at` atomic into a rolling signature
  (`Session::last_output_at`, no vt100 lock); a change means new output, so the
  terminal repaints immediately.
- **Status transitions** mark the UI dirty: `refresh_session_statuses` requests
  a redraw when a session's status/activity/notification actually changes
  (a quiet `Busy → Waiting` produces no output, so the output detector can't
  catch it).
- **Everything else time-driven** — the live clock/metrics, cursor blink, an
  expiring status toast — is covered by `FORCE_REDRAW_INTERVAL` (250 ms): if
  nothing flagged the UI dirty, `App::should_redraw` still paints once the floor
  elapses.

The loop still spins every ≤10 ms (poll input, check output, `tick`), but the
*expensive* work — layout, the vt100 `PseudoTerminal` render, panel rebuilds —
is skipped when idle.

**Why**: The previous loop called `terminal.draw` unconditionally, ~100 fps,
even on a completely idle screen. That is the single largest idle-CPU cost in a
TUI that may sit untouched for long stretches with several sessions open.
Demand-driven rendering drops idle paints from ~100 fps to ~4 fps (the floor)
while keeping input and output repaints immediate — so responsiveness is
unchanged and idle CPU drops by ~25×.

**Rejected**:

- *Per-widget dirty tracking* — far more invasive and bug-prone (every state
  mutation must flag the right widget); the coarse app-level flag plus a time
  floor captures the same win with a fraction of the surface.
- *Lower fixed frame rate (e.g. 20 fps always)* — still wastes CPU when idle and
  adds latency to input/output; the floor only fires when nothing else did.

**Correctness net**: dirtiness is deliberately over-approximated (any input, any
status change → repaint) and the 250 ms floor guarantees nothing time-driven
stays stale longer than a blink. The black-box test (`tests/tui_e2e.rs`) still
asserts the first frame, every post-keystroke frame, and that a reflow repaints
without clearing the screen.

**One pass rides along after each paint**:
`App::paint_outer_hyperlinks` re-emits the frame's OSC 8 hyperlinks so the
outer terminal can offer its own open-link gesture (see the Clickable URLs
section of `docs/FEATURES.md`). It is bound to the *painted* frames — ratatui
rewrites the cells, so the escapes must follow each draw — and is gated on
`HyperlinkTable::is_empty()` **before** it computes layout or extracts screen
rows, so a session whose agent never printed a link pays a single emptiness
check per frame. The pane half (a `url:` node, `ClickVerb::Url`) walks the
hitboxes the paint just recorded, which costs one `split_once` per target — no
allocation unless a role actually is a verb — and reads cells only for the
targets that are one. When links are present the scan is bounded (the newest 128
runs × the visible rows) and the writes are a handful of short `queue!`s per
visible run, bracketed in DECSC/DECRC so the caret the frame just placed is put
back rather than left wherever the last run ended.

---

## ADR-P2: Deterministic perf counters as the regression gate

**Choice**: Performance regressions are caught by **counting**, not timing.
`MetricsState::perf` (`PerfCounters` in `src/app/metrics_state.rs`) holds
wall-clock-free `u64` counters bumped at the render/tick hot paths:

| Counter | Meaning |
| --- | --- |
| `frames_rendered` | `App::view` ran (a frame painted) |
| `redraws_requested` / `redraws_skipped` | loop iterations that painted vs. skipped |
| `status_refreshes` | `refresh_session_statuses` passes (one per tick) |
| `ordered_sessions_rebuilds` | session-list order rebuilt vs. served from cache |
| `parser_locks_render` | central pane locked a vt100 parser to render (one per terminal frame) |
| `automation_entries_built` | automations-pane entry list built |
| `hook_state_loads` | `refresh_session_statuses` actually reloaded the persisted hook columns (`load_hook_states`) — gated on a `data_version` change (ADR-P6), so it stays flat while idle |
| `external_poll_checks` / `external_poll_reloads` | `poll_external_changes` ran its cheap `PRAGMA data_version` check / found a change and did a full shared-state reload |
| `review_builds_dispatched` / `review_builds_applied` | code-review diff builds handed to the background worker / applied back on the UI thread (ADR-P8) |
| `restore_seed_prefetches` | restore history captures prefetched in parallel, one per matched pane (ADR-P9) |
| `agent_meta_syncs` | a session's OSC title/notification actually re-read (gated on the reader thread's meta generation, ADR-P10) |
| `data_version_checks` | the status refresh actually ran its `PRAGMA data_version` read (throttled ~10×/s, ADR-P10) |

`hook_state_loads` is the regression gate for ADR-P6: it climbs once at startup
and then only when an external `session signal` commits, instead of ~1 per tick.
`external_poll_reloads` stays 0 with no other writer. Tick-driven counters are
asserted in the `#[test]` units in `super::tests`
(`perf_hook_states_cached_across_idle_ticks`,
`perf_hook_states_reload_on_external_change`,
`perf_external_poll_never_reloads_without_external_writes`), not the render-path
acceptance harness (which skips `tick`).

The acceptance harness (`src/app/acceptance.rs`) asserts on
`App::perf_counters()` — e.g. *idle iterations skip the paint*, *the session
order is rebuilt exactly once across three idle frames*, *a session-set change
invalidates the cache exactly once*. These run in the normal `cargo nextest run
--all` and gate CI.

**Why**: Wall-clock benchmarks are flaky on shared CI runners — a GC pause or a
noisy neighbour turns a green build red. A counter assertion (`redraws_skipped
== 4`) is exact and reproducible, and it proves the *mechanism* (work was
skipped) rather than a wobbly proxy (it was fast today).

**Rejected**:

- *Timing assertions in CI* (`assert!(elapsed < X)`) — flaky; deleted before
  they were written.
- *criterion / divan micro-benches in the gate* — see ADR-P5.

---

## ADR-P3: Cache the session-list ordering, keyed by a content signature

**Choice**: `compute_session_order` (`src/ui/project_list.rs`) groups, sorts,
and nests the session list. Its output is a pure function of exactly four
per-session fields — `repo_display_names` (grouping), `display_order` (sort),
`id` and `parent_session_id` (nesting) — plus the session count/order, and
**never** of status. `App` caches the computed `SessionOrder` keyed by
`App::session_order_signature` (a hash of just those fields). On a frame where
the signature is unchanged, the cache is reused via
`OrderedSessions::from_order`, skipping the grouping HashMap, the two sort
passes, the nest recursion, and the group-label allocations. The cheap O(n)
remap of refs / match positions / `active_index` still runs each frame (those
vary independently).

**Why**: While an agent streams output the screen repaints every frame, and the
left panel was rebuilding the full ordering on each one even though sessions
rarely change. The signature is strictly cheaper than the order it guards (a
hash vs. HashMap construction + sorts + recursion + allocations), and being
content-derived it is **self-invalidating**: any change that alters the order
alters the hash, so there is no manual "mark dirty" call site to forget.

**Rejected**:

- *An explicit generation counter bumped at every mutation site* — correctness
  depends on instrumenting every add/remove/reorder/reparent/external-sync path;
  one miss is a stale-list bug. The content hash can't miss.
- *No cache* — measurable waste during active output for larger session lists.

---

## ADR-P4: What was deliberately *not* optimized

Measuring first also means **not** adding complexity where the data says it
won't pay off.

- **Scrollback round-trip** (`src/ui/terminal_view.rs`): reading the total
  scrollback via `set_scrollback(MAX)` → read → restore looks like a per-frame
  double-write, but in vt100 0.16 `set_scrollback`/`scrollback` are **O(1)**
  (a clamped field assignment). With ADR-P1 throttling it now runs ~4×/s when
  idle. Caching it would add interior-mutability/borrow complexity for no
  measurable gain. Left as-is (it rides along with `parser_locks_render`).
- **Automations-pane entries** (`src/app/view.rs`): rebuilt each render
  (`automation_entries_built`). The only repeated parse is `humanize_cron`, and
  the countdown portion *must* stay live, so a cache would only memoize the
  schedule string. Automations are typically a handful, and ADR-P1 bounds the
  frequency — not worth the split. Left as-is.
- **vt100 parser lock contention** (`src/app/view.rs` render vs.
  `src/agent/backend.rs` reader thread): the reader locks the parser per output
  chunk; the UI locked it per frame. ADR-P1 collapses idle-frame UI locks to
  near zero (`parser_locks_render`), so the contention window shrinks for free.
  Cloning the vt100 screen for lock-free rendering was rejected — the screen is
  large and the copy would cost more than the brief lock it removes. The UI
  already scopes the lock tightly (lock → render widget → release).

---

## ADR-P5: Benchmarks and profiling live outside the PR gate

**Choice**: The gating automated perf tests are the counter assertions
(ADR-P2). Heavier measurement is **opt-in and local**:

- **Time-to-first-frame**: launch with `THURBOX_PERF_LOG=1`; `run_loop` logs one
  `startup …` line to `~/.local/share/thurbox/thurbox.log` with a **phase
  breakdown** that sums to roughly `first_frame_ms` —
  `config_init_ms` (config-file loads + local backend ready), `db_open_ms`,
  `theme_activate_ms` (persisted-theme lookup + custom-theme publish),
  `extension_heal_ms` (self-heal + built-in hooks wiring + agents reload),
  `app_new_ms` (`App::new`: keybindings load, settings snapshot, channels),
  `restore_ms` (the synchronous local-session restore — remote backends restore
  on background threads, off this phase; see ADR-P7), `heartbeat_ms` (arming
  the automation-heartbeat tmux window), and `first_frame_ms` (total to first
  paint).
  When restore is the long pole, the same flag also emits per-backend
  `restore_discover` lines (`discover_ms`; for a remote backend the line comes
  from its background thread) and per-session `restore_adopt` lines
  (`adopt_ms`) so the *sequential* restore can be attributed. Note `restore_adopt`
  covers both restore paths — **adopt** (a live tmux pane is re-attached) and
  **respawn** (no live pane matched, so a fresh agent is launched); on a cold
  socket (e.g. after a reboot) every session respawns. For the adopt path, an
  `adopt_split` line (in `TmuxBackend::adopt`) further breaks `adopt_ms` into
  `capture_ms` (the independent `tmux capture-pane` subprocess — the only part
  that could run in parallel across sessions) and `connect_ms` (the
  control-mode attach). This split was the deciding measurement for
  parallelizing restore: the control-mode connection is serialized by a single
  mutex held across each command's full round-trip
  (`TmuxBackend::with_control`), so `connect_ms` is inherently sequential and
  only `capture_ms` can be overlapped — which ADR-P9 now does (on the startup
  restore path `capture_ms` reads ≈ 0 and a `restore_capture_prefetch` line
  reports the overlapped batch).
  Off by default — never affects normal runs or the smoke test; the timing reads
  are gated on the flag so there is zero overhead otherwise.
- **Binary size**: the non-gating `binary-size` CI job
  (`.github/workflows/ci.yml`) builds `--release` and records `thurbox` /
  `thurbox-cli` sizes to the job summary + an artifact. It is intentionally
  **not** in `all-checks.needs`, so it never blocks a merge; it just makes
  growth visible. The release profile is already tuned (`opt-level = 3`,
  `lto = true`, `codegen-units = 1`, `strip = true`).
- **Local profiling**: `cargo flamegraph --bin thurbox` (build with the
  `release-with-debug` profile for symbols) for CPU; `cargo bloat --release
  --crates` for size attribution. Neither is a dependency — run them ad hoc.

**Why**: criterion/divan pull a large transitive dependency tree, and
`cargo deny check licenses` (a **gating** CI job with a strict allowlist) would
have to vet all of it. For micro-benchmarks of two pure functions
(`compute_session_order`, `compute_layout`) that the analysis shows are not
bottlenecks, that cost isn't justified — the counter tests already gate
regressions without flakiness or new dependencies.

**Rejected**:

- *criterion in `dev-dependencies` + a gating bench job* — dependency-license
  and flakiness cost for little signal.
- *A startup-time CI gate* — startup is dominated by tmux/agent spawn and
  machine variance; a hard threshold would be flaky. The opt-in log line is for
  local investigation instead.

---

## ADR-P6: Reload the session-status hooks only on a `data_version` change

**Choice**: `refresh_session_statuses` (`src/app/mod.rs`) used to run
`Database::load_hook_states` — an indexed scan of the `sessions` table — on
*every* tick (~100×/s) to derive each session's status. It now caches the hook
rows (`App::cached_hook_states`) and reloads them only when the DB's
`PRAGMA data_version` moves since the last load (`App::hook_states_version`).
The pragma is an in-memory counter read (no table access), so the per-tick cost
drops from a full scan + row mapping + UUID parsing + HashMap build to a single
integer compare. The per-tick *derivation* (spinner, the output-quiescence
`working → Idle` fallback, done/seen logic) still runs every tick against the
cache, so status latency is unchanged. `load_hook_states` itself uses
`prepare_cached` so the reload, when it happens, skips the SQL re-parse.

Two writers don't move *this* connection's `data_version`, so they're handled
explicitly: the deferred `seen_at` marks are applied **write-through** into the
cache (otherwise a just-acknowledged `done` session would re-derive to `Done`
next tick), and the restart path's `clear_hook_state` calls
`App::invalidate_hook_state_cache` (forces a reload). External
`thurbox-cli session signal` writes come from another connection and *do* bump
`data_version`, so they're picked up on the next tick as before.

Alongside this, `Database::initialize` (`src/storage/schema.rs`) sets the
WAL-friendly performance pragmas `synchronous = NORMAL`, `cache_size = -8000`
(8 MB), `mmap_size = 64 MB`, and `temp_store = MEMORY`.

**Why**: ADR-P1 made *rendering* demand-driven, but `tick` still ran every
≤10 ms and re-scanned the sessions table for hook state each time — pure waste
on the overwhelmingly common idle tick where nothing signalled. The
content-derived `data_version` gate is self-invalidating for cross-process
writes (the common case) and can't miss them; the two same-connection writers
are few and explicitly handled.

**Rejected**:

- *Tie the reload to the 250 ms sync poll* (`poll_external_changes`) — would add
  up to 250 ms of latency to a status change (blocked/working/done), a visible
  regression; the dedicated per-tick `data_version` read keeps ~10 ms latency.
- *A second `has_external_changes`-style cursor* — that method mutates the
  shared `last_data_version` used by the sync poll; reusing it would make the
  two consumers steal each other's change edges. A read-only `data_version()`
  avoids the coupling.

---

## ADR-P7: Restore remote-backed sessions in the background

**Choice**: `App::restore_sessions` (`src/app/mod.rs`) partitions the resumable
sessions by `is_remote_backend(backend_type)`. Local sessions keep the
synchronous discover + adopt path (sub-second). Each **remote** (`ssh:<host>` /
`wsl:<distro>`) backend is readied + discovered on its **own thread** — all
hosts in parallel — via `App::start_remote_restore`; its sessions wait in
`App::remote_restore` and are adopted on the main thread by
`App::poll_remote_restore` (a `tick` step) once the host reports. A
late-arriving adoption restores the user's prior selection instead of stealing
focus, and a session already adopted meanwhile (e.g. by the DB sync) is
skipped. An unknown-host backend still leaves its sessions un-adopted, exactly
like before. The per-backend `restore_discover` perf line is emitted from the
background thread; `restore_ms` in the `startup` line now covers only the
local, synchronous part.

**Why**: readying a remote backend means an ssh connect + remote tmux server
bring-up — observed at 15–30 s per host on real hardware, unbounded when a host
is powered off. With sessions persisted on two lab hosts, the first frame took
~50 s (the hosts were probed **serially**, before `run_loop` ever painted).
The expensive part needs no `&mut App` — only `ensure_ready()` + `discover()`
on an `Arc<dyn SessionBackend>` (`Send + Sync`) — so it moves off-thread
wholesale; adoption itself reuses the control-mode connection the thread
already brought up and is cheap on the main thread.

**Rejected**:

- *Parallelizing the synchronous restore across hosts* — turns 50 s into
  max-per-host (still 15–30 s, still unbounded for a down host) and keeps the
  first frame hostage to the slowest machine.
- *An ssh `ConnectTimeout` default* — caps the down-host case but does nothing
  for a reachable-but-slow host, and silently changes user ssh behavior.
- *Adopting on the background thread too* — `restore_single_session` mutates
  `App` (session list, wizard state on the respawn path); shipping a built
  `Session` across the channel would split that invariant for no measured win.

---

## ADR-P8: Build code-review diffs off the UI thread

**Choice**: Opening the code-review view (`Ctrl+X`/`F7`) and switching its
target used to run the whole git pipeline **synchronously in the key
handler** — per repo: base resolution (`branch_exists` + `list_branches` +
`default_branch`), the target-picker commit listing, and the diff itself,
each a `git` subprocess and **each an ssh round-trip for a remote session**.
Measured via the `code_review_build` slow op (ADR-P11): seconds of frozen UI
on a remote host. Now `toggle_code_review` does only the cheap gather
(session id, host, worktree list, label dedup), installs the review in a
`loading` state — the pane opens instantly with a "Building diff…"
placeholder — and hands the git work to a `spawn_blocking` worker
(`build_review_open` / `build_review_retarget` in `src/app/code_review.rs`,
via the shared `BackgroundTask` fire-and-poll shape). `App::poll_review_build`
(a `tick` step) applies the result **by session id** into `App::code_reviews`,
so a review closed (or switched away from) mid-build simply drops the result.
One build runs at a time; a second open/retarget while one is in flight is
refused with a toast.

Gate: `review_builds_dispatched` / `review_builds_applied` +
`perf_review_open_never_builds_on_ui_thread`,
`perf_review_build_result_applied_via_poll`,
`review_build_for_closed_review_is_dropped` (`src/app/mod.rs` tests).

**Why**: this was the largest *interactive* stall in the app, and the inputs
are all owned/cloneable data (`ReviewRepo`, `HostDef`, the target), so the
work moves off-thread wholesale with the same pattern the codebase already
uses for git stats and worktree creation.

**Rejected**:

- *Queueing a second build behind the in-flight one* — a rapid open→retarget
  would apply two results in sequence for no benefit; the refuse-with-toast
  is simpler and the loading state makes it obvious.
- *An async-aware diff stream (progressive per-repo fill-in)* — more moving
  parts for a build that is fast locally; revisit only if multi-repo remote
  reviews prove slow *after* this change.

**Superseded by `kernel::diff`.** Everything named above (`toggle_code_review`,
`App::code_reviews`, `build_review_open`, and all four gate tests) went with
`src/app`; the reasoning survives because the successor is the same shape. The
kernel's `DiffStore` computes on a worker and publishes into the snapshot, which a
plugin reads as `thurbox.diffs[<session>]` — `pending` / `failed` / `ready`, with
`files`, `body`, `truncated`, `raw_bytes` and `untracked_omitted`. Five things
about it are worth stating because each was wrong once:

- **The base is `sessions.base_branch`, never the session's own branch.** The loop
  passed `SessionRow.branch` — a session's *own* worktree branch — so the range was
  `<own-branch>..HEAD`, which is empty. Every worktree-backed session published a
  `ready` diff with no files: a confident wrong answer, and the inversion of the
  intent, since the sessions that *have* a base were the ones showing nothing. The
  snapshot now carries `base_branch` beside `branch` (a bulk read, on the schedule
  the hook columns already use) and `None` still means "diff the uncommitted
  changes".
- **The request follows the selection, not the focused pane.** It was driven by
  `focused_session`, re-derived each frame from the focused plugin's *session
  surface* — so only a pane drawing a terminal ever asked for a diff, and a pane
  whose job is showing one could never be handed it.
- **The file list is not capped; only the body is.** `files` was derived from the
  capped text, so it listed only what fit — 310 of 433 files on this repository's own
  diff, with totals to match, and `truncated` (which is about *bytes*) gave a reviewer
  no way to know the navigation aid ended early. It now comes from `git diff --numstat
  -M -z` plus `--name-status -M -z`: two cheap commands, ~12 KB for four hundred files
  against a 4 MiB body, joined on the new path. A failure to list fails the diff rather
  than reporting a partial list as complete.
- **The uncommitted diff has to include untracked files.** `git diff HEAD` cannot
  show a file git has never been told about, and a new file is the most common thing
  a coding agent produces — so a session with **no** base branch, which is exactly
  the scratch worktree someone watches an agent work in, reported "no changes" after
  three files had been written. v1 had the same gap; the consequence is worse here
  because that is the default target. `git::working_diff_on` now folds each
  untracked file in as `git diff --no-index -- /dev/null <path>`, which emits an
  ordinary `new file mode` patch and needed nothing downstream to change: the
  numstat record arrives in the *rename* shape (empty path, `/dev/null`, real name),
  which `parse_changed_files` already handled. The body, the counts and the statuses
  come back from **one** call so they cannot disagree about which files they covered.
  The rejected alternative is the instructive one: a temporary index
  (`GIT_INDEX_FILE` + `git add -A` + `git diff --cached`) gets everything in one
  process and **writes loose objects into the repository being reviewed** — for a
  pane refreshing every few seconds against a worktree an agent is editing, a reader
  mutating what it reads. Bounded at `git::UNTRACKED_FILE_CAP` (200) since each file
  costs a process, and what was left out is reported as `untracked_omitted` rather
  than folded into `truncated`: a short *list* and a cut *body* are different
  failures and read differently.
- **A cached diff can be discarded.** `command("diff", { session })` invalidates,
  and the next frame recomputes. Without it a diff was computed once per session per
  process and never again — a diff frozen at first sight while the agent kept
  writing, which is exactly the "cached answer with no age" mistake this document
  warns about two sections down.

---

## ADR-P9: Prefetch restore's history captures in parallel

**Choice**: The sequential local-session restore adopts one session at a time,
and ADR-P5's `adopt_split` measurement shows each adopt is `capture_ms` (an
independent `tmux capture-pane` subprocess) + `connect_ms` (the control-mode
attach, serialized by the connection mutex — inherently sequential). Restore
now runs all matched panes' captures **in parallel** up front
(`App::prefetch_capture_seeds`, a bounded `std::thread::scope` fan-out capped
at 8 concurrent subprocesses) and passes each seed into the adopt:
`SessionBackend::adopt` takes `seed: Option<Vec<u8>>` (`None` = capture
inline, exactly the old behavior — used by mid-run adopts, shell-pane
re-adoption, and the remote restore path) and the new
`SessionBackend::capture_history` exposes the capture as its own trait method.
With N sessions the restore's capture cost drops from `N × capture_ms` to
roughly one `capture_ms`; `adopt_split` now logs `capture_ms ≈ 0` on the
startup path, and a `restore_capture_prefetch` line (`sessions`,
`prefetch_ms`) reports the overlapped batch.

Gate: `restore_seed_prefetches` (one per prefetched pane) +
`perf_restore_prefetches_capture_seeds` (`src/app/mod.rs` tests — a recording
backend asserts every adopt received a prefetched seed and the capture ran
exactly once per session; count-based, no timing).

**Why**: startup time is dominated by restore once a few sessions exist, and
the capture half is the only slice that parallelizes without touching the
control-mode serialization ADR-P5 documents.

**Rejected**:

- *Parallelizing whole adopts* — `connect_pane` shares one control-mode
  connection guarded by a mutex held across each command round-trip; threads
  would just queue on it.
- *Unbounded capture fan-out* — a 50-session restore would fork 50
  subprocesses at once; the cap keeps the burst bounded with the same
  wall-clock win.

---

## ADR-P10: Cut the idle tick's per-session churn

**Choice**: two reductions in `refresh_session_statuses`' ~100 Hz work, both
gated by counters:

- **Agent meta generation gate.** `apply_session_status_fields` called
  `session.agent_title()` + `session.notification()` for every session every
  tick — 2·N mutex locks and up to 2·N `String` clones at ~100 Hz, almost
  always re-reading unchanged values. The reader thread's `TermSignals` now
  bumps a shared `meta_gen` atomic **after** each title/notification write,
  and `Session::sync_agent_meta` re-reads the mutexes only when the
  generation moved (one relaxed/acquire atomic load per session per tick
  otherwise). Status derivation itself still runs every tick, so
  blocked/working/done latency is unchanged; a generation observed mid-write
  only delays the text by one ~10 ms tick (the counter is bumped after the
  value lands). Gate: `agent_meta_syncs` +
  `perf_agent_meta_cached_across_idle_ticks` /
  `perf_agent_meta_resyncs_on_change`.
- **Throttled `data_version` read.** The ADR-P6 cache still ran its `PRAGMA
  data_version` `query_row` every tick (~100 rusqlite round-trips/s). The
  read now runs every `HOOK_VERSION_CHECK_TICKS` (10 ticks ≈ 100 ms) — except
  when the cache was explicitly invalidated (a remote hook event or restart
  wrote on our own connection, which the pragma can't see; those check
  immediately). Worst case an external `session signal` displays ~100 ms
  late instead of ~10 ms — far below the 250 ms coupling ADR-P6 rejected as
  visible. Gate: `data_version_checks` +
  `perf_data_version_read_is_throttled` (and
  `perf_hook_states_reload_on_external_change` now ticks through a full
  throttle window before asserting).

**Why**: with the render loop demand-driven (ADR-P1) and the hook reload
cached (ADR-P6), these two were the largest remaining per-tick costs, and
both scale with session count. Neither changes any user-visible latency
budget.

**Rejected**:

- *Sharing `poll_external_changes`' cursor for the hook check* — still the
  ADR-P6 rejection: `has_external_changes` mutates the shared
  `last_data_version`, so two consumers would steal each other's edges.
- *Gating the whole status derivation on the generation* — the
  output-quiescence `working → Idle` fallback and the spinner are
  time-driven; they must run every tick regardless.
- *Deferred follow-ups* (measure first via the new observability):
  extension self-heal gating on a fingerprint (watch `extension_heal_ms`),
  and an adaptive idle poll interval for the 100 Hz loop itself (idle CPU
  was not a reported pain point; the tick is now cheap).

---

## ADR-P11: Runtime observability — timing histograms, slow ops, perf window

**Choice**: The deterministic counters (ADR-P2) now have a **runtime
observability layer** on top — wall-clock stats that are **display/logging
only** and never CI-asserted (the counters remain the sole regression gate):

- `App::perf_counters()` is a runtime accessor (previously `#[cfg(test)]`).
- `MetricsState.timings` (`src/app/metrics_state.rs`) holds two hand-rolled
  fixed-bucket `DurationHistogram`s — `terminal.draw` duration per painted
  frame and `App::tick` duration per iteration — plus a 16-slot `SlowOps`
  ring of named synchronous UI-thread operations (`SlowOp { name, ms, tick }`).
  No new dependencies: the histogram is ~40 lines with power-of-two µs buckets
  (250 µs → 1 s + overflow), good enough to answer "is a frame 1 ms or 30 ms".
- **Gating**: the hot-loop `Instant` reads run only while
  `App::perf_timing_active()` — `THURBOX_PERF_LOG` set (cached at
  construction) or the perf HUD open — so a normal run pays a single cached
  bool check per loop iteration, keeping ADR-P5's zero-overhead promise.
- **Slow ops**: `App::time_op(name, f)` wraps rare, user-triggered synchronous
  operations (the code-review build/retarget/reload, and `App::update` outliers
  as `input_dispatch`). Always measured (call sites are not the hot path):
  ≥ 5 ms lands in the ring, ≥ 100 ms also logs a `slow op` warning — so an
  interactive stall is attributable even when nobody was watching.
- **Steady-state reporting**: under `THURBOX_PERF_LOG`, every 1000 ticks
  (~10 s) `App::tick_perf_window` logs one `perf_window` line — counter
  **deltas** for the window (`PerfCounters::delta`), frame/tick p50/p95/max,
  and the window's slow ops — then resets the per-window timing state. The
  one-shot `startup` line is unchanged.
- **The perf HUD** (`src/ui/perf_hud.rs`, F12, `[features] perf_hud`): a
  floating, non-modal overlay with the same counters/percentiles/slow-ops,
  refreshed by the existing 250 ms forced-redraw floor.
- **External inspection**: while timing is active the TUI also publishes a
  JSON snapshot (counters + percentiles + slow ops + the startup phases) into
  the SQLite `metadata` table (`perf_snapshot` key, ~every 5–10 s), read by
  **`thurbox-cli perf`** (`--json` for machine output). Publishing is gated on
  timing being active because each write bumps *other* thurbox connections'
  `data_version` (a full shared-state reload on their next poll) — an idle,
  default-config instance must never churn that row.

**The v2 implementation** (the bullets above name v1 modules that went with
`src/app`; the design carried over, the file names did not):

- `kernel::perf` owns the whole layer — `DurationHistogram` (same fixed µs
  buckets), `SlowOps` (same 16-slot ring), `Timings`, `Startup`, and
  `snapshot_json`, which is the single owner of the published JSON shape.
  `cli::perf` only renders whatever that produces, and
  `tests/kernel_perf.rs` pairs the two so a key renamed on one side fails
  rather than printing a silent zero.
- **Three histograms, not two.** `frame` (the `terminal.draw` call) and `tick`
  (one iteration's non-blocking work) are joined by **`republish`** — the
  per-frame rebuild of every `thurbox.*` table. They are recorded so they
  *decompose* rather than nest: `tick` is taken before the paint, so an
  iteration is roughly tick + republish + frame + the input wait. Telling the
  table rebuild apart from the painting is the difference between "frames are
  expensive" and knowing why, and it is the number ADR-P14 should be read
  against.
- **Gating** is `App::perf_timing_active` — `perf_log` (cached from
  `THURBOX_PERF_LOG` at construction) or the HUD being open — checked once per
  iteration, so a default run pays one bool.
- **Slow ops** wrap `interface_reload` (the whole reload: rebuild, sources,
  declarations) and `input_dispatch` (a keypress, which runs plugin Lua and so
  is where a slow plugin is felt). ≥5 ms rings, ≥100 ms also warns.
- **Startup phases are v2's own**: config, DB open, theme activate, extension
  heal, heartbeat, **`ui_build_ms`** (building the Lua interface — a cost v1
  did not have) and `first_frame_ms`. There is no `restore_ms`: v2 has no
  synchronous restore phase.
- **The subscriber had to be restored.** v2's TUI shipped without one at all,
  so every `tracing` call in the process — the panic hook's included — was
  dropped rather than written. `main` now installs the daily rolling
  `thurbox.log` appender v1 had, which is what makes the lines below exist.

**Why**: the counters gate regressions in CI but were invisible in a live
build, and they deliberately count rather than time — so a user-perceived
stall ("opening review froze for 3 s") had no signal at all. The histograms
and slow-op ring answer *how long*, the `perf_window` line answers *what is
the app doing while idle*, and both stay out of CI so ADR-P2's no-flaky-timing
rule holds.

**Rejected**:

- *Always-on timing* — two `Instant::now()` calls per ≤10 ms loop iteration is
  cheap but not free, and observability nobody asked for shouldn't tax every
  run; the opt-in gate costs one bool.
- *A timing dependency (hdrhistogram etc.)* — same licensing/vetting cost
  ADR-P5 rejected for criterion; the fixed-bucket histogram is sufficient.
- *CI assertions on the new timings* — explicitly ruled out; ADR-P2 stands.

---

## ADR-P12: Make the whole new-session flow non-blocking, and show it working

**Choice**: `Ctrl+N` had one phase left on the UI thread and one with no
feedback; both are fixed, and the flow now reports itself for its full
duration.

*Off-thread* (the R1 recommendation from the 2026-07-09 investigation):

- **Branch listing.** `start_branch_selection` ran `fetch_pending_repos`
  (`git fetch` — a network round-trip **per repo**, ssh-wrapped for a remote
  host), `list_branches_on`, and `ordered_branch_list` inline in the key
  handler. Since the repo picker closes *before* it runs, the freeze happened
  with **no modal, no toast and no repaint** on screen — measured at ~2 s
  locally, unbounded on a slow network, ~5 s per unreachable host. It now
  dispatches a `BackgroundTask` (`App::branch_list`) and the selector opens in
  `App::poll_branch_list`, mirroring `worktree_create`/`poll_worktree_create`.
- **Backend ready-up.** `build_spawn_inputs` called `backend_for`, whose
  `ensure_ready()` is an ssh connect + remote tmux bring-up (15–30 s on a slow
  host, per ADR-P7) — and it sat in the *prelude* of `do_spawn_session_async`,
  so a remote spawn blocked the loop before the worker was ever dispatched.
  Split into `App::select_backend` (registry lookup, cheap, UI thread) and the
  free `ensure_backend_ready` (blocking), which the async path now calls
  **inside** its `spawn_blocking` closure. The synchronous `do_spawn_session`
  (automations/tasks/restore, which need the id back immediately) calls it on
  its own thread by contract.

*Feedback* — `App::pending_spawn` (`PendingSpawn` + `SpawnPhase`), which lives
for the **whole wizard**: from the repo being chosen until the session is live,
across both the background phases *and* the modals between them. It is cleared
only when the session lands, the flow errors, or the user Escs out (every wizard
modal's cancel path calls `abandon_pending_spawn`, or a cancelled flow would
strand a placeholder row forever).

- A **placeholder row** in the session list (`ui::project_list`), so the session
  appears the moment the wizard is confirmed rather than after a slow shell-out.
  It carries no `SessionInfo` and records **no hitbox** — so selection indices
  stay a valid range over the real sessions and the monkey test's invariants are
  untouched. Its label upgrades as the wizard learns it: the repo, then the
  session name. It renders **inside the repo group it will land in**
  (`pending_spawn_slot`), at the end of that group — where the real row will
  appear, since a new session has no `display_order` and sorts after its ordered
  siblings. A repo with no rows yet brings its own header rather than floating
  loose at the bottom. `PendingSpawn.repo_display_names` (resolved once when the
  repo is chosen — `git::repo_display_name` can shell out on a cache miss, so it
  must not run per frame) mirrors what `SessionInfo::repo_display_names` will
  carry, so the pending row and the real one group identically. Because the row
  is inserted rather than appended, the widget's item indices are offset past it
  when mapping back to session indices (hitboxes and the selected item).
- An **animated badge** in the status row (`⠹ NEW  Creating worktree(s)… feat/x
  · 14s`), reusing the `Ctrl+S` sync spinner's surface, with an elapsed counter
  so a long wait reads as progressing rather than hung.
- `SpawnPhase::is_working()` splits the three shell-out phases from
  `Configuring` (a wizard modal is open). A `Configuring` spawn keeps its row and
  badge — the session is still on its way — but shows a **static `◌`**, no
  elapsed counter, and does not drive `advance_spinner_frame`: spinning a spinner
  while the app waits on the *user*, and timing how long they take to answer,
  would both be lies. A working phase animates at ~8 fps instead of resting on
  the 250 ms redraw floor.

*Extension (repo-picker path entry).* The picker itself later gained the same
treatment: its three remote round trips — the path-browser directory listing
(Tab), the Enter-commit path check (exists + git-ness, one trip), and the
`Alt+P` parent scan — each run on a `BackgroundTask` worker drained by
`App::poll_repo_picker` in `tick_core` (`src/app/repo_picker.rs`), replacing
the synchronous `list_dir_on` probes that ran in the key handler. The modal
stays fully interactive while a fetch is in flight (spinner rows/labels);
supersession is handled by restarting the task (the orphaned worker's send
fails harmlessly) plus a per-picker-instance `repo_picker_gen` stamp so a
result that outlives its modal (Esc + reopen) is dropped — including a parent
import's DB writes. Local targets compute inline (`std::fs` is instant, and
the acceptance harness runs without a Tokio runtime). Listings are cached per
`(picker instance, dir)`; an in-browser Tab bypasses the cache as an explicit
refresh. Gates: `poll_repo_dir_listing_*`, `poll_repo_path_check_*`,
`poll_repo_parent_import_*` (`src/app/mod.rs` tests);
`repo_picker_browser_*` (`src/app/acceptance.rs`).

*Re-entrancy.* Unfreezing the flow makes its window **interactive**, which is a
new hazard: `branch_list` and `worktree_create` carry `new_session` state across
a thread boundary, so a second `Ctrl+N` in that window would overwrite the repo
the in-flight job is resolving — repo A's branch list would land on repo B's
config, cutting a worktree from a branch B may not have. `start_new_session`
therefore refuses re-entry while `new_session_in_flight()`, and drops anything
the caller staged (a task-spawn's `pending_task_prompt` would otherwise be
`take()`n by the session already in flight — the wrong one).

**Why**: the phases were *already* mostly backgrounded (ADR-P8's shape), but the
progress was announced with a `status_message` — and those expire after
`STATUS_MESSAGE_TIMEOUT` (5 s). A `git worktree add` on a large repo runs well
past that, so the toast vanished mid-job and the app looked idle: the user's
report was "creating takes time and I have no info on screen that creation is in
progress". Progress that outlives a 5 s timer cannot *be* a status message, hence
a separate piece of state that lives exactly as long as the work.

Gate: `spawn_progress_outlives_the_status_message_timeout`,
`spawn_progress_reports_elapsed_time`, `spawn_placeholder_row_is_not_selectable`,
`spawn_placeholder_replaces_the_empty_state` (`src/app/acceptance.rs`);
`poll_branch_list_*`, `indicator_survives_every_wizard_modal`,
`escaping_a_wizard_modal_drops_the_indicator`,
`new_session_is_refused_while_the_branch_list_is_in_flight`
(`src/app/mod.rs` tests).

**Rejected**:

- *Keeping the repo picker open in a `loading` state instead of a placeholder
  row* — the row doubles as the answer to "where did my session go", and the
  modal would block the rest of the TUI for no gain.
- *Exempting `status_message` from expiry while a spawn is in flight* — the
  smaller diff, but it overloads a transient-toast slot with durable state, and
  a frozen string still reads as hung. The elapsed counter is what proves
  liveness.
- *Making the placeholder selectable* — it has no `SessionId` to select, and a
  clickable row that resolves to nothing is worse than an inert one.
- *Clearing the indicator whenever a wizard modal opens* (the first cut) — it
  made the session blink in and out of the list between phases, which reads as
  "it disappeared". A session being created exists from the moment you commit to
  it; the phase only changes what it's waiting on.

---

## ADR-P13: A frame is a Lua call per pane, so the loop settles hard

**Choice**: Keep v1's demand-driven loop (ADR-P1) and make it stricter, because a
frame now costs more. Every visible pane is a Lua call returning a table, which is
converted to nodes (`kernel::convert`) and painted (`kernel::paint`); v1's frame was
a Rust function writing into a buffer.

So the loop paints only when something changed, or when `FORCE_REDRAW_INTERVAL`
(250 ms) elapses, with `MIN_FRAME_INTERVAL` as the floor between two paints. What
marks the screen dirty:

- any input, a resize, a reload, a modal or focus change
- a worker result (`terminals`, `commands`, `diffs`, `metrics`, `repos`, `runs`,
  `updates`)
- **new agent output** — `Terminals::output_generation` is summed each iteration and
  compared. This is v1's `detect_output_redraw`, and it is what stops a printing
  agent being drawn at the 250 ms floor
- **a plugin's tree differing from the last one it returned.** `draw` keeps
  `last_trees[index]` and only marks the frame changed when the new tree differs.
  This is what makes an animating plugin work without a plugin-visible animation
  API: a spinner's tree differs each frame, so it keeps the loop awake by itself,
  and a static pane costs one comparison

**One frame is deliberately NOT a diff: a reflow.** When the arrangement places a
slot at a different rect — a side column opening or closing, the search strip, the
message band taking its row — every cell of that frame is printed rather than only
the ones the diff believes moved. The diff is correct exactly while ratatui's idea
of a cell's width matches the terminal's, and grapheme clusters exist where it
cannot: a regional-indicator flag is two columns to `unicode-width` and a different
number to several emulators, so a pane that closes leaves glyphs in the column that
replaced it, and they survive until something else happens to repaint those cells.
`kernel::paint::normalize_ambiguous_width` removes the one disagreement that can be
removed (the emoji-presentation selector, stripped from every painted cell); this
covers the rest by spending one full repaint on an event the user just caused. It is
bounded by how often a layout moves, which is a keypress — not a frame.

**How that full repaint is asked for matters as much as that it happens.**
`kernel::paint::force_full_repaint` marks every cell of the finished frame
`CellDiffOption::AlwaysUpdate`, so the flush that follows prints all of them. The
obvious instrument, `Terminal::clear`, is the wrong one on three counts: it flushes
an erase on its own and the repaint only arrives in the *next* flush, so the whole
interface visibly blinked on every pane toggle; it queries the terminal for the
cursor position first, which is a synchronous round trip on the input stream, on a
keypress; and a reflow never needed an empty terminal in the first place — it needed
every cell printed, which is what the marks say and nothing more. The cost is that
the marks land in the buffer the next frame is diffed against, so the frame after a
reflow prints in full too: one extra full write, invisible, on the same keypress
bound.

**Why the settling had to get stricter**: two things marked every frame changed
unconditionally and so pinned the loop at the frame cap — an open float, and a
non-empty text selection. With the creation wizard up, that meant rebuilding every
Lua tree ~60 times a second for as long as it was open. A float now settles by
comparing its own tree *and rect*, like a pane; a selection is already in the
buffer and moving it takes a mouse event, which marks dirty on its own. The perf
HUD keeps its unconditional mark, deliberately — its counters do move every
iteration, and it says so.

**Freshness is a property of a cached answer, not of having one.** The mistake this
codebase kept re-inventing: `surveyed` recorded that a backend had *ever* been
listed, so a session created since was judged against a listing that predated it and
relaunched — killing the agent its own spawn had just started. `GitStats::known`
never expired, so a diffstat froze at its first reading. A `run` marked only
`Pending` started a process per frame once its answer went stale. A failed branch
fetch stuck for the process lifetime. Each is now a TTL, an in-flight marker, or a
generation counter. **If you add a cache to the loop, give it an age**; the review
that found these is in the history, and they were one field each.

**Bounds belong to the kernel, not the plugin.** A plugin may ask for a program
(`kernel::runs`) every frame — that is the documented pattern, because a fresh answer
is a map lookup — so the store refuses a duplicate while the answer is fresh *or*
while a run for that key is in flight, caps output with truncation flagged, times
out, and runs four at a time with the rest queued. The Lua VM has an instruction
budget and a memory ceiling (`tests/kernel_limits.rs`), so a plugin cannot spin the
frame either.

**Rejected**:

- *Caching converted nodes across frames* — the tree is the plugin's output and
  cheap to compare; caching it would need invalidation the kernel cannot see into.
- *A plugin-facing animation API* (`request_frame`) — the tree diff already gives
  it, without a second way to keep the loop awake or a plugin that forgets to stop.
- *Rendering panes in parallel* — one Lua VM, and the isolation model
  (`enter` stamps the current plugin per call) depends on calls being serial.

---

## ADR-P15: What a v2 frame actually costs, and the three cuts taken

**Context**: v2 was measured against v1.8.7 under identical synthetic load
(three agents each printing 30 lines/s, both binaries running *simultaneously*
so machine noise hits them equally). v2 painted **fewer** frames than v1 and
still used 2.5x the CPU, so the gap was never frame *rate* — it was the price
of one frame. Attribution, release builds, with the observability of ADR-P11:

| | v1.8.7 | v2 before | v2 after |
|---|---|---|---|
| CPU | 14.6% | 36.5% | 30.7% |
| frames/s | 47 | 37 | 40 |
| **CPU per frame** | **3.1ms** | **9.9ms** | **7.7ms** |

Inside a v2 frame the draw was ~75% and `republish` ~25%; inside the draw, the
Lua->node **conversion** cost more than running every plugin's Lua put together
(session list: 897us of Lua, 3006us of conversion, for 187 nodes — ~14us *per
node*). ratatui's own flush was never the problem (~0.6ms).

**Choice**: three cuts, each measured on its own before/after pair:

- **One `pairs` pass per node** (`convert::Fields`) instead of ~25 individually
  keyed lookups — `read_size` alone was five, and each hashes its key and
  crosses into the VM. Conversion 3.9ms -> 1.9ms a frame; **frame cost -25%**.
  This is the same fix `read_style_field` already carried one level down.
- **Borrowed error paths** (`convert::Crumb`) instead of
  `&format!("{path}[{}]", index + 1)` per node — a `String` per node, growing
  with depth, for text read only when something is malformed (~15% of
  conversion).
- **Link extraction only for surfaces on screen** (ADR-P14's stamp still
  applies on top). Scanning every live pane's whole grid cost ~1.2ms a frame
  with three sessions, for answers nothing could use; **1152us -> 320us**. This
  restores v1's rule, which asked only the active session.

`publish` also moved to `raw_set` on tables it created moments earlier and
pre-sizes the ~30-field session row, which is worth ~5%.

**Since closed** by ADR-P16, which added the change-signals this names as the
prerequisite and then gated both the published groups and the pane renders on
them.

**Rejected**: *throttling the repaint rate* — v2 already paints fewer frames
than v1; capping it further trades responsiveness for a number. Note the
converse, which the measurements make plain: because repaints are
**output-driven**, a cheaper frame becomes *more frames* rather than less CPU
(the conversion cut took 25% off the frame but only 9% off CPU). Per-frame work
is still the right target — it is what makes the interface able to keep up —
but the CPU it returns is bounded by `MIN_FRAME_INTERVAL`.

---

## ADR-P16: Make a frame cost what changed, not what exists

**Context**: ADR-P15 left v2 at 2.4x v1's CPU and 2.5x its cost per frame, with
the gap identified as the Lua boundary — running each pane, converting the table
it returns, and rebuilding every `thurbox.*` group, all once per painted frame.
Instrumenting the tree diff showed why that was avoidable: under load the session
list produced a **byte-identical tree on 200 of 200 renders**, and the agent pane
repainted because its *surface* moved rather than its tree. The loop was proving
the work wasted only after paying for it.

**Choice**: give every published source a change-signal, then spend it twice.

- **Signals** live with the mutation, never with the caller:
  `SnapshotStore::version`, `Themes::version`, `Registry::version`,
  `Terminals::meta_version` and `failed_version`, plus one loop-side
  `data_epoch` fed by the `changed` flag each worker store's `poll` already
  returns. Deriving that last one from an existing return value means it cannot
  drift from it.
- **Gated publish**: each `thurbox.*` group names the versions it is built from
  and is rebuilt only when one moves. The outer table is still assembled fresh
  every frame, so a gating mistake can produce a stale *group* but never a torn
  table. Keys are compared exactly (`[u64; 4]`), not hashed — a collision here
  would serve a stale group, and "astronomically unlikely" is the wrong
  guarantee for a wrong answer nobody can see.
- **Pure panes**: a pane may declare `pure = true`, asserting its render is a
  function of the published tables and its context. The kernel then reuses the
  tree it last returned, keyed on the epoch, the rect, focus, an animation tick
  and the plugin-state version. **Opt-in**, because a render may write `store`
  (the search strip does) or animate, and neither is visible from outside the
  VM; an undeclared pane behaves exactly as before.

| | v1.8.7 | before P15 | after P15 | now |
|---|---|---|---|---|
| CPU | 15.6% | 36.5% | 31.0% | **23.1%** |
| CPU per frame | 3.1ms | 9.9ms | 9.1ms | **5.2ms** |
| gap to v1 (CPU) | — | 2.5x | 2.0x | **1.5x** |

**Four things the implementation taught, each caught by a test rather than by
review:**

- **`taken_at_ms` is published**, and `widgets.relative_time` renders it, so the
  snapshot's generation has to move on every refresh — not only when rows
  change — or "5s ago" freezes. A change-signal is about what is *read*, not
  about what feels significant.
- **A pure render may read `store`/`state`**, which handlers write. Without that
  in the key, the agent pane's per-session tab survived the keypress that
  changed it. Seven tests failed; the hole was real.
- **Writing an unchanged value must not count as a change.** The search strip
  re-states one `store` key every frame; treating that as a write moved the
  state version 40 times a second and invalidated every cached tree. With it,
  the saving was 0%; without it, 27%. Every signal here compares before it
  stores, for exactly this reason.
- **`thurbox.commands` is read too, and accepting one had no signal.** The
  in-flight list is published every frame, but only its *completion* side moved
  the data epoch (`poll_command_bus`); submitting a command moved nothing. The
  session list drops the row a `delete` names as soon as it is accepted, and
  being pure it was handed the tree built before the command existed — so the
  deleted row survived until the animation clock ticked 125ms later, or until
  the delete finished, which is the wait the feature exists to avoid.
  `dispatch_tracked` now moves the epoch as the command is accepted. Asserted by
  `accepting_a_command_must_move_the_epoch_to_reach_a_pure_pane`, which settles
  the pane first — a pane that is never cached cannot go stale, so a test that
  skips the settle proves nothing.

**Rejected**:

- *Opt-out caching* (`animated = true` to escape) — a performance change must
  not be able to break a third-party pane that was never touched, and the
  failure would be a pane that stops updating rather than an error.
- *Read-tracking* (proxy `thurbox.*`, key on fields actually read) — the most
  precise answer and needs no annotation, but it does not solve side effects
  either and is a far larger change. Opt-in purity composes with adding it later.
- *Per-pane dependency sets* — the measured waste is frames where *nothing*
  moved, so a coarse epoch captures it.
- *Throttling the repaint rate* — v2 already paints fewer frames than v1.

**Follow-up, measured after the above landed.** The counters it added showed the
gate barely working at rest: `renders skipped` was **3 of 284** on an idle
interface. Three fixes:

- **The animation clock was free-running.** It was read straight from
  `ctx.elapsed` as `floor(elapsed * 8)`, so it advanced 8 times a second whether
  or not anything was moving — and at the 4fps idle floor that invalidated every
  pure pane on every frame. It now lives in `Epoch::animation`, advanced by the
  loop only while something animates (a `working` session, a command in flight).
  This was a bug against this capability's own requirement that an idle
  interface rebuild nothing, not a tuning choice.
- **An adaptive poll timeout.** The loop blocked in `event::poll(10ms)`
  regardless, costing 94 wakes a second at rest — about half of idle CPU. After
  `QUIESCENT_AFTER` with nothing happening it waits `IDLE_TICK` (50ms) instead.
  This costs **no** input latency: `event::poll` returns the moment an event
  arrives. What it delays is noticing what does *not* wake the thread — new
  agent output, a worker result — and at rest there is none of the first. Still
  well inside the 250ms redraw floor.
- **`platform` is constant** and was rebuilt 33 times a second.

| | v1.8.7 | after P16 | now |
|---|---|---|---|
| CPU (loaded) | 15.6% | 21.2% | **19.4%** |
| CPU per frame | 3.1ms | 5.4ms | **4.2ms** |
| CPU (idle) | 2.33% | 4.83% | **3.57%** |

A fourth followed: at rest the snapshot's generation moved every
`REFRESH_INTERVAL` (400ms) purely because `taken_at_ms` was re-stamped, which
capped how long any pure pane could stay cached. That field is **published to
the second** now (`taken_at_stamp`), because its only reader is
`widgets.now_ms` feeding `time_ago`, which floors the difference to whole
seconds — so the precision being dropped is precision no reader could ever see.
Quantising the published *value* rather than delaying the signal is what keeps
"a plugin never reads a stale published value" true. The unconditional touch it
replaced was also covering git stats landing in the same branch, so
`attach_git_stats` now reports whether it changed a row. Idle 3.57% -> 3.33%.

**Still open** *(closed by ADR-P18)*: the Lua boundary genuinely paid, the node
paint, and `publish`'s remaining volatile groups (hover, commands, inventory,
diffs, links, content, metrics, runs). Note throughout that repaints are
output-driven, so a cheaper frame partly becomes *more* frames rather than less
CPU.

---

## ADR-P17: Separate the frame floor for output from the one for input

**Context**: with ADR-P16 landed, a frame costs ~2.6ms and the interface still
took ~19% of a core to show one agent printing 30 lines a second. Measuring the
whole process tree against the same workload run bare (agent in a tmux pane, no
thurbox) put bare at **1.20%** — agent 0.56, tmux 0.64. Two of those components
are unavoidable for thurbox: the same agent, plus its own inner tmux (~0.9%),
which is what makes a session survive a restart. So thurbox starts at roughly
bare's *entire* cost before it paints anything, and parity is not a reachable
target; the question is only how much of the rest is waste.

The waste was the frame rate. `MIN_FRAME_INTERVAL` (16ms) was applied to every
reason a frame was owed, so an agent printing 30 lines a second drove ~60 paints
a second. Typing has to feel instant. Watching a log scroll does not.

**Choice**: a second floor, `OUTPUT_FRAME_INTERVAL` (33ms), used when the only
thing owing a frame is new agent output. `App::input_dirty` marks the other
kind — a keypress, a resize, a worker result someone asked for — and those keep
the 16ms floor. It is raised only through `App::note_input`, which sets it with
`dirty`: a site that set just `dirty` would pace a keystroke at 33ms and look
like nothing more than a slow terminal. The three intervals also only mean
anything in relation to each other — output slower than input, both under the
forced-redraw floor — and getting that wrong is silent in either direction (the
split becomes a no-op, or the 250ms floor quietly becomes the real cadence), so
the ordering is asserted rather than left to the definitions.

Swept across the interval, one agent at 30 lines/s, 200x50:

| floor | fps | interface | terminal | total |
|---|---|---|---|---|
| 16ms | 62 | 19.08 | 0.84 | 21.24 |
| **33ms** | **30** | **12.40** | **0.62** | **14.40** |
| 50ms | 20 | 11.12 | 0.52 | 13.08 |
| 100ms | 10 | 8.30 | 0.34 | 10.18 |

Most of the saving arrives by 30fps and the curve flattens after; below it the
scroll begins to look stepped. The terminal's own cost falls with it, because
thurbox hands it fewer updates.

**Also**: the surface paint stopped clearing its whole rect first. `swap_buffers`
resets the frame buffer before every draw, so there is nothing stale to erase
across frames, and a live terminal covers its own rect — the `Clear` was a second
full-grid write of ~9,000 cells for a frame about to overwrite them. It is kept
for the branches that do *not* cover their rect, each owning its own clear rather
than being cleared by the caller: `kernel::terminal::clear_uncovered` for a grid
still smaller than its rect a frame after a resize, and the detached and notice
widgets for themselves. `normalize_ambiguous_width` also rejects a cell on a
length compare (`VARIATION_SELECTOR_16_LEN`, derived from the selector rather
than written out) before any substring search, since U+FE0F is three bytes and
almost every cell is one.

**Measured, paired, 3 runs each:**

| | interface | total |
|---|---|---|
| before | 16.80 | 18.72 |
| the paint changes alone | — | ~18.4 |
| the output floor alone | 11.36 | 13.20 |
| both | 11.12 | **12.92** |

**-31% overall.** Worth recording honestly: the paint changes were predicted at
~13% and delivered **~2%**. `Clear` writes blank cells, which is cheap beside the
terminal widget's per-cell read-convert-style work that still happens; and any
per-frame saving is halved once the frame rate is. The frame *rate* was the
money, not the frame *cost*.

**Rejected**:

- *Chasing bare* — arithmetically impossible while sessions live in tmux, since
  the agent plus that tmux already exceed bare's total.
- *Row-level surface diffing* — vt100 exposes `rows_diff`, but it emits terminal
  byte streams for a multiplexer forwarding to a real terminal, not row indices a
  ratatui cell buffer could use. It needs a per-row change signal that does not
  exist yet.
- *A lower floor than 30fps* — 20fps buys 1.3 more points and 10fps another 2.9,
  against a visibly stepped scroll. Not worth it as a default.

**Still open**: `publish` rebuilds 15 of its 25 groups every frame; painting is
still whole-frame even for panes whose tree the cache knows is unchanged; and
the vt100 surface repaint (~800us) is now the largest single line item in a
frame.

---

## ADR-P14: Publish once per input batch, and gate every screen read on a stamp

**Choice**: `App::republish` — the call that rebuilds every `thurbox.*` table Lua can
read — runs **once per event batch** rather than once per event, and the three reads
inside it that touch a screen or the filesystem are gated on something that says
whether the answer could have changed.

It ran per event because a handler has to read something current. It does; nothing
between two events of one batch can change what it would say, since the snapshot is
refreshed at the top of the iteration and a command a handler queues is drained on
the next one. What that cost, per keystroke — and a held-down key is 30 or more a
second, each one draining in the same batch:

| Read | What it did per event | What gates it now |
|---|---|---|
| `Terminals::links` | walked every cell of **every** live session's grid, building a `String` per row, to find OSC 8 targets and bare URLs | that session's `output_stamp` — the same atomic the redraw signal reads |
| `Terminals::screens` (search content) | re-read every grid again, capped at `CONTENT_LINE_CAP` | `output_generation`, plus the existing "is anything asking" check |
| the interface inventory | `read_to_string` + digest of **every file** in the interface directory, and a `plugins.lock` TOML parse, to answer "is this file still the one that was trusted" | a `trust_stale` flag set by `refresh_sources`, which every path that changes the directory or a grant already calls |

The rows of the inventory are still assembled every publish: which pane is *on
screen* depends on the frame, and that half is a set lookup. Only the file reads
behind them are cached, which is ADR-P13's rule applied — the cached answer carries
the thing that makes it current, not merely the fact that it exists.

**Measured by**: the `renders` counter is unchanged (the same trees are built); what
falls is the work between them. There is no counter for "publishes", which is the
honest gap here — the change was reasoned from what the reads do, and the reads are
the same ones ADR-P13 already treats as per-frame costs.

**Rejected**:

- *Publishing lazily, on the first `thurbox.*` read from Lua* — the publish builds one
  table; making it per-field would put a Rust callback on every field read, which is
  the cost this avoids, spread thinner.
- *Not publishing before input at all* — a handler would read the previous frame's
  world, and a key pressed on what a frame showed has to act on what that frame showed.

---

## ADR-P18: Close the volatile groups, and every cache carries its age (2026-08-23)

**Context**: ADR-P16 gated the big published groups and ADR-P17 split the frame
floors, leaving a named list of "remaining volatile groups" rebuilt on every
publish — the worst being `diffs`, which republished up to `MAX_DIFF_BYTES` of
body line by line, every frame, forever once computed. Beside them stood a set
of per-frame costs the earlier ADRs had not reached: a pure-pane cache *hit*
still deep-cloned its tree twice, the frame buffer was cloned whole per paint,
the OSC 8 repaint re-walked a vt100 grid per frame per linked session, the
arrangement re-ran through Lua per frame, and one cache — `DiffStore` — still
violated ADR-P13's rule outright, holding its first answer for the life of the
process. Off the frame path, a claiming `DELETE` ran on the UI thread every
loop iteration (a write-lock acquisition per 10 ms, with a 5 s busy-timeout
stall as its worst case), and every ssh invocation paid a full handshake.

**Choice**: one pass, four families, no new mechanism — the existing signals
were enough:

- **Every published group is gated.** `diffs`, `links`, `content`, `commands`
  and `metrics` key on the data epoch (which moves on every worker result and
  command transition, and deliberately never on agent output — so a streaming
  turn reuses them all) paired with the snapshot version; the creation flow's
  three parameterised reads pair the epoch with an FNV digest of the question,
  which also gives their tables the stable identity the flow's own memoization
  keys on; the interface inventory keys on a digest of its rows; `hover` reuses
  one shared empty table while nothing is hovered; the roots map follows the
  snapshot version. The arrangement result is cached on
  (size, reloads, epoch, state version, status rows) — everything `layout.lua`
  can consult.
- **The tree path stopped cloning.** `Rendered.node` is an `Rc<Node>`: a pure
  cache hit is a refcount bump, the settle diff short-circuits on pointer
  identity before walking a node, the last-tree stores skip when the held tree
  is already equal, and decoration clones only when a decorator claims the
  slot. Painting borrows (`to_line` yields `Line<'a>`), the frame buffer is
  read in place instead of cloned to end a borrow, the band settle stores a
  hash instead of the cells, and the per-plugin index lists (focusable,
  floating, slot members, slot modes) are built once per reload instead of
  scanned with an allocation per query per frame.
- **Every cache now carries its age.** `DiffStore` holds `(at, diff,
  refreshing)` mirroring `repos`: a settled answer expires after `DIFF_TTL`
  (failures retry sooner) with the old answer still published while the
  recompute runs. The screen-row extraction the link scan, click resolve and
  OSC 8 repaint all want is computed once per output stamp and shared.
- **The world got cheaper to ask.** ssh multiplexes by default
  (`ControlMaster=auto` behind the existing first-occurrence-wins contract);
  worktree stats read one `status --porcelain=v2 --branch` instead of 5–8
  processes; untracked diffs render counts and patch from one invocation;
  `hosts.toml` loads once per process; pane pids resolve through one
  `list-panes` per backend per second instead of one serialized round trip per
  session; the focus-request `DELETE` rides the snapshot's `data_version`
  gate; `upsert_session` is one transaction instead of N+5 autocommits; the
  bookmarks worker reopens the database without replaying the schema pass.

**Consequences**: under a streaming agent the publish is now group reuse plus
the one pane whose surface moved — the terminal grid conversion ADR-P17 names
is the remaining line item. The rules this doc states are finally uniform:
every group is change-gated, and there is no cache without an age (the one
deliberate exemption, `REPO_NAME_CACHE`, keys on a repository's origin URL,
which does not move within a process's lifetime). The failure mode to guard in
review is unchanged from ADR-P16: a store that mutates without moving its
signal — which is why kernel-side `store` writes now bump the state version
exactly as the Lua path does, the bug the focus request used to hit.

---

## ADR-P19: A remote round trip is not a local one, and neither is unpaced (2026-08-28)

**Context**: shared sessions (ADR-24) gave the loop two new pieces of periodic
work against a host, and both inherited a cadence sized for a local process.

`Terminals::sync` used to exclude remote rows from window discovery outright —
"a remote spawn drives control mode and records the real pane id, so a remote
row is not something a window name can fix". Sharing made that false: a row
mirrored from a host's database names the pane the host reported, or none, and
its window is found on the host's server by name like a local one. So the
filter went. What went with it was the *reason* it was there — a remote
listing is `ssh <host> tmux list-windows`, not a fork on this machine. The
throttle behind it was a single process-wide `discovered_at`, so every backend
with an unresolved row was surveyed at `DISCOVERY_INTERVAL` (500 ms). A remote
row that cannot attach — a host that is down, a mirrored row whose pane is
gone — therefore held a backend permanently unresolved and cost **two ssh
commands a second, forever**: `ensure_ready()` (the connect) and `discover()`
(the listing). `ATTACH_RETRY_INTERVAL`'s 20 s backoff, which exists for exactly
this host, guards only the attach; discovery ran beside it unpaced, and each
survey re-issued the connect the instant the previous one gave up.

The mirror worker had two of its own. It reopened the database with
`Database::open` every `MIRROR_INTERVAL` per host — the constructor that
replays the migrations, re-issues the WAL pragma (**which takes the write
lock**) and runs both retention prunes, against the database the loop is
reading. That is the cost `kernel::repos` had already paid and fixed. And
`host_cli::usable` caches a `Yes` for the process lifetime — correct, a host's
CLI does not change under us — so a host reachable at its first probe and down
since keeps a usable verdict, and every pass ran its ssh out to the connect
timeout, six times a minute.

**Choice**: pace each piece of work by what it actually costs.

- **Discovery is throttled per backend**, at that backend's own interval:
  `DISCOVERY_INTERVAL` (500 ms) locally — it is also how fast a fresh local
  spawn finds its window, so it stays tight — and `REMOTE_DISCOVERY_INTERVAL`
  (5 s) over ssh or `wsl.exe`, where nothing is waiting on it the way a local
  spawn is. `discovery_due` holds *when a backend may next be surveyed* rather
  than when it last was, stamped when a survey **returns** — so the interval
  separates one round trip from the next however long it took, and a slow one is
  not re-issued the instant it gives up. `refresh_mirrors` paces itself the same
  way; in both, the in-flight set is what holds a second attempt off while the
  first is still out.
- **A survey that learned nothing backs off to `ATTACH_RETRY_INTERVAL`** — it
  could not ready its backend or could not list it, and the next one a moment
  later learns the same nothing. A down host is now probed on one schedule
  instead of two. `Terminals::forget` clears that backoff, for the reason it
  already clears the attach failure: a local restart records no pane id, so the
  session is resolved by name and a held-off listing would freeze it just as
  long.
- **The mirror worker uses `Database::open_existing`**, following
  `kernel::repos`: the TUI ran the schema pass at startup.
- **A mirror pass that could not run backs off to `MIRROR_RETRY_INTERVAL`**
  (60 s). The verdict cache and the pass cadence are separate questions; the
  pass is the one that pays for an unreachable host.

**Consequence**: a down host costs a bounded handful of ssh attempts a minute
rather than a continuous stream, and the mirror stops taking the database's
write lock on a timer. The rule this closes out for remote work is ADR-P13's,
one level up: a cache carries an age, and so does a *probe* — anything issued
on a timer against a host needs an interval sized to the round trip, and a
failure needs an interval of its own. Pinned by
`kernel::terminal::tests::{a_remote_backend_is_surveyed_on_its_own_cadence,
a_survey_that_learned_nothing_backs_off_to_the_attach_retry,
a_mirror_pass_that_could_not_run_backs_its_host_off}`.

---

## ADR-P20: A link on a scrolling screen must not un-gate the frame (2026-08-29)

**Context**: ADR-P16 made a frame cost what changed, and ADR-P18 closed the last
ungated groups. Both rest on the data epoch, and both state the same rule out
loud: the epoch moves on a worker result or a command transition and
**deliberately never on agent output**, so a streaming turn reuses `diffs`,
`links`, `content`, `commands` and `metrics` whole and every `pure` pane keeps
the tree it last returned.

It did not hold. `refresh_links` is gated on the surface's `output_stamp`, which
is exact for a screen that has *stopped* and no gate at all for one that has
not: a printing agent moves it on every frame. The scan then walks the whole
vt100 grid, and — because the URLs on a scrolling screen sit on different rows
each time — the compare-before-store below it found a real change and called
`note_published_change()`. So agent output moved the epoch after all, once per
painted frame, for anyone whose agent prints something link-shaped. Which is
every coding agent: a PR link, a docs link, a `file://` path.

The effect is invisible in the frame and plain in the profile. Measured with
`scripts/dev/perf-run.sh`, 19 sessions at 255x62 with three agents printing 30
lines a second — the same run twice, differing only in whether the printed lines
contain a URL (`-u 0`):

| output | CPU | frame p50 | republish p50 | pane trees served from cache |
|---|---|---|---|---|
| with URLs | 8.32% | 4000us | 500us | 179 |
| no URLs | 5.00% | 1000us | 250us | 3487 |

A URL in the output cost **66% more CPU** and took the tree cache from 3487 hits
to 179. The gating was not degraded; it was off.

**Choice**: a second gate on the scan, an age — ADR-P13's rule, which this path
never had. `LINK_SCAN_INTERVAL` (250ms) bounds how often a surface whose screen
is *still moving* is rescanned; the output stamp keeps serving a screen that has
settled exactly, and for free, forever. The stamp is deliberately not recorded on
a skipped pass, so the next publish after the interval does the scan and a
settled screen converges back onto the stamp.

250ms because nothing acts on a link's *position* faster than that, and nothing
reads the published map to act at all: a click resolves against the live grid
(`Terminals::url_at`) and the OSC 8 repaint recomputes from `cached_rows`
(`hyperlink_paints`). `thurbox.links` exists for a plugin to draw, and no bundled
pane reads it. So the interval bounds staleness in the published map and nothing
else.

**Measured**, the same run before and after, each paired with its own no-URL
control so the machine's mood is not part of the claim:

| | with URLs | no-URL control | penalty | frame p50 | cached trees |
|---|---|---|---|---|---|
| before | 8.32% | 5.00% | **+66%** | 4000us | 179 |
| after | **5.27%** | 4.57% | **+15%** | 2000us | **3347** |

**-37% CPU under load**, and the cost of a URL in the output falls from two
thirds of the frame budget to a seventh. It is reduced rather than removed, and
the residual is the honest arithmetic of the choice: four paced rescans a second
still move the epoch four times, against thirty. Removing it entirely would mean
either not publishing link positions at all — `thurbox.links` has no bundled
reader, but an out-of-tree pane may — or a per-group invalidation the tree cache
cannot express, since its key is the whole `Epoch`. Neither is worth it for the
seventh; both are written down here so the next person does not rediscover them.

**Consequences**: the rule ADR-P16 and ADR-P18 both state is now true of the one
path that broke it. The failure mode to take from this is not "links were slow" —
it is that **a per-frame recompute whose answer legitimately changes is a way to
move a change-signal that no reviewer is looking for**. The compare-before-store
that guards `store` writes is not enough on its own: it asks whether the value
moved, and here it truly had. What was missing was the other question, whether it
was worth asking yet.

Pinned by `coordinator::publish::tests::*` for the pacing rule (including that
the interval must sit between the output frame floor and the forced-redraw floor,
or it is a no-op that reads as tuned), and measurable at any time with
`just perf -n 19 -p 3 -s 255x62` against the same run with `-u 0`.

**What moves the epoch now**, attributed at each call site over a 25s run of that
same load, so the next person starts from a measurement rather than a guess:
`refresh_links` 117 (the four paced scans a second, the residual above),
`Metrics::poll` 36, `DiffStore::poll` 7 — around six a second between them,
against the ~30 publishes a second a streaming turn drives. The snapshot version
moves on roughly one publish in twenty. So the epoch stands still for about three
publishes in four, and the pure trees and the float probes are served from the
cache together at that rate — they share the key, so they hit and miss as one,
which is worth knowing before reading `renders_skipped` as a per-pane figure.
The remaining movers are each a worker result someone asked for, which is what
the epoch is *for*.

---

## ADR-P21: Animation belongs to whoever reads the clock (2026-08-29)

**Context**: `advance_animation` advances a shared clock while any session is
`working` — the normal state of a machine with an agent running — and the clock
is in the pure-pane cache key. So *every* pure pane re-rendered eight times a
second to move a spinner glyph: the centre pane, which draws a terminal surface,
and all three closed float probes, which draw nothing at all.

The harness could not see it, because `sh` runs no status hook and nothing ever
reported `working`. `perf-run.sh -w N` now signals N sessions the way a hook does
(`THURBOX_SESSION` plus `session signal`), which made it measurable — and it was
the largest single cost left:

| 19 sessions, 3 printing, 255x62 | CPU | pane trees from cache |
|---|---|---|
| `-w 0` | 6.00% | 2922 |
| `-w 4` | **9.08%** | 1886 |

**+51%** for one glyph per working row. Unlike ADR-P20 the signal is *honest* —
the session list really does depend on the clock. What was wrong is that every
other pane paid for it.

**How everyone else does it.** Worth reading before choosing, because the answer
is unanimous and thurbox had neither half of it:

- **Textual** — a spinner widget calls `self.set_interval(1 / 60, self.refresh)`
  on *itself*, and `refresh` marks that widget dirty. Rich's `Spinner` derives
  its frame from `console.get_time()`, so the frame follows the clock while the
  *invalidation* follows the widget.
- **Bubble Tea** — `bubbles/spinner` returns its own `TickMsg` command. The whole
  view is re-rendered, but the renderer line-diffs against the previous frame and
  writes only changed lines, so the cost lands at the output layer.
- **fidget.nvim** — an `Anime` is `fun(now: number): string`, polled by fidget's
  own heartbeat (which idles when there is no work, and never exceeds ~40Hz), and
  it repaints fidget's own float.
- **lualine** — does not trigger redraws at all; neovim redraws the statusline on
  its own events, and a timer-driven `:redrawstatus` refreshes *only* the
  statusline.

One principle behind all four: **the clock invalidates only its reader**. Three
of them get that coupling for free, because the thing that reads the clock is
also the thing that asks to be redrawn. thurbox's panes do not ask — the kernel
calls them — so the coupling had to be recovered some other way.

**Choice**: recover it by *observation*. `ctx.elapsed` is served through the
render context's metatable instead of being set as a field, so asking for it is
something the kernel can see; a pure pane's cached tree records whether the
render that built it read the clock, and the animation tick is compared only for
trees that did (`CachedTree::answers`). `__index` fires only for absent keys, so
every ordinary field (`width`, `height`, `focused`, `frame`, `name`, `slot`)
stays a raw read and pays nothing, and the metatable is built once per VM rather
than per render.

**Measured**, the same paired runs:

| 19 sessions, 3 printing, 255x62 | before | after |
|---|---|---|
| `-w 0` (nothing animating) | 6.00% | 5.32% |
| `-w 4` (four spinners) | **9.08%** | **5.96%** |
| animation penalty | **+51%** | **+12%** |
| trees from cache at `-w 4` | 1886 | 2725 |

The penalty is the honest figure — it is internally paired, where the two `-w 0`
readings differ by run-to-run noise on a shared machine. The residual +12% is the
session list, which is *supposed* to re-render: it is the pane with the spinner
in it.

**Rejected — a declaration**, in either direction, which is what this looked like
before the prior art was read:

- Defaulting to "does not animate" recovers nearly everything and silently
  freezes any third-party spinner whose author never read the release note. A
  wrong-direction failure with no error anywhere is the class of bug this
  document exists to record.
- Defaulting to "does" is safe and recovers only the panes thurbox ships.
- Taking the spinner out of the tree and painting it as a decoration needs no
  declaration, but reworks the session list and only moves the cost.

Detection beats all three because it cannot be wrong in either direction: the
flag is read from the render that produced the very tree being cached, so a pane
that starts or stops reading the clock re-keys itself on the render where it
does. There is no frame in between on which a stale tree could be served —
asserted in `kernel_frame_cost::a_pane_that_starts_reading_the_clock_is_keyed_on_it_from_then_on`,
alongside the two directions and one test against the real bundled interface.

**Consequences**: one visible change to the plugin contract — `elapsed` is not a
key of `ctx`, so it does not appear in `pairs(ctx)`. Nothing iterates a render
context, and `docs/PLUGINS.md` now states the rule the mechanism creates: reading
`ctx.elapsed` is what subscribes a tree to the animation tick, so read it where
you animate and not at the top of a render that usually draws nothing moving.

---

## Measuring: the bench and the load harness (2026-08-29)

Two instruments, because "a frame costs 2ms" and "thurbox costs 8% of a core"
are different claims and neither implies the other. Both live outside the PR
gate, per ADR-P5.

**`cargo bench --bench frame_cost`** — the pieces of a frame, against the real
`ui/` and a synthetic snapshot. It models what `draw` does rather than what the
plugin list contains: it resolves the arrangement and renders only the panes an
arrangement of that size actually *places*, plus the float probe every frame
pays. Rendering every loaded plugin instead reported the closed search strip as
the second most expensive pane in the interface, which it is not — it occupies no
slot, so `draw_slots` never reaches it.

It reports whole frames (settled, snapshot moved, animation tick), then the
parts, then per placed pane, then what the caches did. `THURBOX_BENCH_SESSIONS`,
`THURBOX_BENCH_WIDTH` and `THURBOX_BENCH_HEIGHT` sweep it — a height sweep is
what separates "the session list costs 435us" from "a visible row costs 9us".

**`scripts/dev/perf-run.sh`** — the whole binary under a reproducible load: real
tmux panes, a real vt100 grid per session, the real loop, in a fully isolated
sandbox with `sh` printing on a timer as the agent. It reports CPU from `/proc`
plus the loop's own `perf_window` line, and keeps the log at
`target/perf-run.log`. The documented instruction before it was "launch it and
leave it idle", which measures the one regime nobody complains about.

```sh
scripts/dev/perf-run.sh                        # 8 sessions, 1 printing, 30s
scripts/dev/perf-run.sh -n 19 -p 3 -s 255x62   # a working machine's shape
scripts/dev/perf-run.sh --idle                 # the settled floor
scripts/dev/perf-run.sh -u 0                   # the control for ADR-P20
scripts/dev/perf-run.sh --no-perf-log          # is the instrumentation the cost?
```

Two traps it now handles, both of which report a plausible number rather than
failing:

- **It must measure its own process.** `pgrep -x thurbox` finds the developer's
  own running thurbox first, and every configuration then reports that instance:
  idle or loaded, one session or twenty, all ~17% of a core. The run is
  identified by its private `XDG_DATA_HOME` in `/proc/<pid>/environ` instead.
  (`pgrep -f "$BIN_DIR/thurbox"` has the matching problem from the other end —
  it also matches `thurbox-cli`.)
- **The TUI starts before the sessions exist.** The v1→v2 consent gate fires for
  a profile with session history and no acknowledgment, and waits for a keypress;
  seeding first left the binary sitting on the gate for the whole run, reporting
  a very restful 0%.

A reading from either is only comparable with another at the same terminal size
and session count, so both pin theirs.

**And on a busy machine, trust the counters over the CPU.** A percentage from
`/proc` is a real measurement of a shared machine: taken while something else was
compiling, the same build measured 5.96% and 7.53% on two runs half an hour
apart. So take a before and an after **back to back**, in one batch, and read
them as a pair — every table in the ADRs above was gathered that way, which is
why they quote a *penalty* (`-u 0` against `-u 12`, `-w 0` against `-w 4`) rather
than a lone number. `renders_skipped`, `groups_reused` and `frames` in the same
output are deterministic and do not care what else is running: for the two fixes
above, cached trees over the same workload went from 154 to 3070 against an
unchanged frame count, which is the claim that holds however loaded the machine
was. `uptime` before believing a percentage.

---

## Investigation 2026-07-09: where the time actually goes

A measurement pass over the render loop, the tick, the draw path, startup, the
database, and the mailbox wake. Ranked by **measured** impact. Anything not
measured is called out under [Honest gaps](#honest-gaps) rather than guessed at.

### Method

- **Machine**: Intel i7-8700K (6C/12T, 3.70 GHz), 31 GiB RAM, Linux
  7.0.14-arch1-1, tmux 3.7, rustc 1.97.0 stable.
- **Build**: `cargo build --release` (LTO, stripped). No number below comes
  from a debug build.
- **Isolation**: every binary ran with `HOME`, `THURBOX_CONFIG_DIR`,
  `THURBOX_DATA_DIR`, `THURBOX_SOCKET` and `TMUX_TMPDIR` redirected to
  throwaway directories, so no measurement touched real config, the real
  database, or the real tmux socket. Database work ran against a **copy** of
  `thurbox.db`; `EXPLAIN QUERY PLAN` was never pointed at the live file.
- **Loop timing**: the built-in instrumentation, not a new dependency —
  `THURBOX_PERF_LOG=1 thurbox` inside a scratch tmux pane, reading the
  `startup` and `perf_window` lines (ADR-P11) from
  `$THURBOX_DATA_DIR/thurbox.log.<date>`.
- **Load generator**: a synthetic agent declared in a scratch `agents.toml` —
  a throttled producer (100 lines/s/session) and an unthrottled one (`yes`) —
  driven at 0, 4 sessions.
- **Database**: `sqlite3` against the copy (544 session rows, 22,726
  `audit_log` rows), 200 iterations per statement.
- **Subprocess costs**: `git worktree add`, `git fetch`, and `ssh` timed
  directly with a monotonic clock, five/three runs each.

Reproduce: build release, export the five env vars above to temp dirs, run
`THURBOX_PERF_LOG=1 thurbox` in a tmux pane, read the log.

### Findings

#### 1. `git fetch` blocks the UI thread for ~2 s on the new-session path

> **Fixed by ADR-P12.** `start_branch_selection` now dispatches to a worker and
> the selector opens in `poll_branch_list`. The measurement below is what
> motivated it.

`App::start_branch_selection` (`src/app/key_handlers.rs:1448`) calls
`fetch_pending_repos` (`src/app/key_handlers.rs:1486`), which runs
`git::git_fetch_on` (`src/app/key_handlers.rs:1491`) synchronously on the UI
thread, once per repo.

Measured against this repository's `origin`: **1776 ms, 1954 ms, 2018 ms**
(three runs). The cost is network-bound and therefore unbounded — for a repo
on a remote host the call is ssh-wrapped, and a single ssh connect to an
unroutable address takes exactly **5014 ms** (`ConnectTimeout=5`,
`src/shell.rs:51`).

Symptom: the TUI stops painting and stops accepting input for ~2 s after a repo
is chosen in the new-session flow, longer on a slow network, and ~5 s per
unreachable host. This is the only multi-second freeze on the ordinary
interactive path. The sibling calls on the same path (`list_branches_on`,
`default_branch_on`, `branch_exists_on`, `list_dir_on`) are local `git` and
cost single-digit ms.

#### 2. A `Spawn` automation creates worktrees and a tmux window inline

`process_automations` runs on the tick. Its `Spawn` arm reaches
`spawn_and_prompt` (`src/app/mod.rs:6156`), which calls
`git::create_or_attach_worktree` at `src/app/mod.rs:6183` (primary repo) and
`src/app/mod.rs:6206` (each extra repo), then the **synchronous**
`do_spawn_session` (`src/app/mod.rs:3660`).

Measured `git worktree add` on this repository: **84, 88, 93, 97, 100 ms**
(median ~93 ms) — roughly six dropped frames at 60 Hz, multiplied by the number
of repos in a multi-repo spawn, before the tmux window spawn is even counted.

The contrast is the point: the **interactive** `Ctrl+N` spawn was already moved
off-thread (`do_spawn_session_async` at `src/app/mod.rs:3733`, drained by
`poll_worktree_create`/`poll_session_spawn` in `tick_core`). The automation
spawn path never received the same treatment.

#### 3. The mailbox wake reports success at a pane nothing is listening to

`send_prompt_now` (`src/agent/tmux.rs`) targets the session's tmux window and
treats a zero exit from `send-keys` as delivery. thurbox sets
`remain-on-exit=on` (`SESSION_OPTS`, `src/agent/tmux.rs:1719`; verified
session-level on the live server), so an agent that exits or crashes **leaves
its window and pane in place**.

Measured against a pane whose process was killed (`pane_dead=1`, window still
listed):

| Target | `send-keys` exit | bytes delivered |
| --- | --- | --- |
| live pane | 0 | 18 |
| **dead pane** (`remain-on-exit`) | **0** | **0** |
| missing window | 1 | 0 |

So `{"woke": true}` meant "tmux accepted the keystrokes", not "an agent
received them". `cli::messages::enqueue_and_wake` set `woke = true` on that
`Ok(())`, and the recipient never acted because there was no process to act.
The message itself was always durably queued — only the liveness report lied.

This is **not** the automation freeze and shares no mechanism with it: no event
loop, no blocking call, no starvation. It is a false-positive liveness signal
in a headless CLI path. Five call sites shared it — the mailbox wake
(`src/cli/messages.rs:300`), `session send` (`src/cli/sessions.rs:299`),
`task run` (`src/cli/tasks.rs:310,333`), and the **headless `Send` automation**
(`src/cli/automations.rs:545,571`), which recorded a `Success` run for a prompt
that went nowhere.

Fixed here: `send_prompt_now` now refuses a dead pane, so all five callers
report the truth. A missing window still surfaces through `send-keys`, because
`display-message` against one exits 0 printing nothing.

#### 4. Output-driven repaint runs at the full loop rate (~100 fps)

ADR-P1's demand-driven paint holds while idle, but any agent output marks the
UI dirty, so during a streaming turn the loop paints on essentially every
iteration.

| Load | frames / ~10 s window | frame p50 | p95 | max | tick max |
| --- | --- | --- | --- | --- | --- |
| idle, 0 sessions | 40 (~4 fps floor) | 0.50 ms | 0.79 ms | 0.79 ms | 0.39 ms |
| 4 sessions, 100 lines/s each | 987 (~99 fps) | 1.00 ms | 4.00 ms | 7.81 ms | 0.49 ms |
| 4 sessions, unthrottled | 1000 (~100 fps) | 1.00 ms | 1.00 ms | 1.51 ms | 0.79 ms |

Under the unthrottled load thurbox held a steady **65.6 % of one core** (RSS 34
MB) and the tmux server **98.7 %**. No frame came close to the 16 ms budget and
**zero slow ops** were logged in any run.

This is a throughput cost, not a freeze. Related but smaller: a `Working`
session forces a repaint every `SPINNER_TICKS_PER_FRAME = 12` ticks (~8 fps,
`src/app/mod.rs:4560`) even when nothing visible changed — dominated by the
output-driven rate above whenever the agent is actually producing output.

### What is fine

Checked, measured, and acceptable — listed so the absence of a finding here
means "looked at", not "not looked at".

- **The tick.** `tick_core` p50 **250 µs**; worst observed max **790 µs**
  (idle), **490 µs** (4 throttled sessions), **786 µs** (flood). Never within
  20x of a dropped frame.
- **The draw.** Diffed, not full: `terminal.draw` double-buffers and flushes
  changed cells only; there is no per-frame `terminal.clear()`, and the `Clear`
  widget is scoped to modals, the perf HUD, and the review pane. Frame p50
  0.5–1.0 ms, worst max 7.81 ms.
- **Startup.** `first_frame_ms` = **46 ms** (no sessions), **67 ms** (4
  sessions), **152 ms** (4 sessions including restore + adopt). Nothing is
  enumerated eagerly that need not be.
- **The database.** The only per-tick statements are `PRAGMA data_version`
  (**0.0062 ms**) and, behind it, `load_hook_states` (**0.014 ms**).
  `EXPLAIN QUERY PLAN` gives `SCAN sessions USING INDEX idx_sessions_active`,
  returning 3 active rows out of 544. `audit_log` (22,726 rows) and
  `automation_runs` are never read from the loop. No missing index; no full
  scan; the ADR-P6 cache does what it claims.
- **The session-order cache (ADR-P3).** Signature is an O(sessions) hash with
  no allocation and is status-independent, so streaming output and spinner
  ticks reuse the cached order.
- **The vt100 lock.** Taken once per painted frame, for the visible pane only,
  for an O(rows x cols) copy. Background sessions' parsers are never locked
  during render.
- **Remote SSH.** Both configured hosts were **up** during this pass
  (`linux-hp` 317 ms rc=0; `windows-hp` connects, returns `ALIVE`) — ssh
  returns 255 on connect failure, and neither did. No **active** session is
  remote: all 3 live sessions are `local-tmux`; the 8 `ssh:*` rows are
  soft-deleted. Backends are registered lazily (`App::select_backend`) and
  readied on background threads (ADR-P7/P12), so a down host cannot block
  `tick_core`. For
  the paths reachable here, "keep the TUI usable when remote SSH hosts fail"
  holds.

### Recommendations

| # | Change | Benefit | Cost | Safe independently? |
| --- | --- | --- | --- | --- |
| R1 | Move the new-session `git fetch` off the UI thread, mirroring `poll_worktree_create` | Removes the only multi-second freeze on the normal interactive path (~2 s, unbounded) | Medium: needs a loading state + a `poll_*` drain | Yes — **done**, ADR-P12 |
| R2 | Route the automation `Spawn` through the existing `do_spawn_session_async` | Removes a ~93 ms x repos + tmux-spawn freeze per fire | Low–medium: the async path already exists | **No** — must edit `src/app/automation.rs`, reserved by `fix/automation-exec-nonblocking` |
| R3 | Refuse a dead pane in `send_prompt_now` | `woke`/run-status stop lying; applies to all five callers | One tmux round-trip per send | Yes — **applied in this PR** |
| R4 | Clamp output-driven repaint to ~30 fps | ~3x less TUI CPU while agents stream (65.6 % -> ~20 % of a core) | Low (one clamp) but needs an input-latency measurement first | Yes — **not done**, see gaps |
| R5 | Skip the spinner repaint when the spinner cell is off-screen | Minor; subsumed by R4 whenever output is flowing | Low | Yes |

R2 is the one that overlaps the in-flight `Exec` fix. Both arms live in
`fire_automation`, so landing them separately would conflict; the
worktree/spawn offload should ride with that branch or follow it.

### Honest gaps

- **The chronic freeze was not reproduced.** Under worst-case synthetic output
  nothing on the UI thread exceeded 16 ms and zero slow ops were logged. If a
  continuous freeze is real, the evidence points *away* from the render/tick
  loop; the tmux server pegging ~99 % of a core (finding 4) and the host
  terminal emulator are the untested candidates.
- **No real agent CLI was exercised.** `HOME` was isolated, so no authenticated
  `claude`/`codex` ran. A real agent's output — full-screen TUI repaints, wide
  ANSI runs — has a different vt100 shape than the synthetic producers used
  here, so finding 4's frame times are a lower bound.
- **No CPU profile by symbol.** `perf`, `cargo-flamegraph` and `valgrind` are
  all absent on this machine and `perf_event_paranoid = 2`; `criterion` is not
  a dependency and there is no `benches/`. Per ADR-P5 none were added just to
  measure. All attribution above is from the built-in counters plus direct
  subprocess timing.
- **The `Exec` automation path was not measured** — reserved for
  `fix/automation-exec-nonblocking`.
- **Remote-session render and status cost is unmeasured**: no active remote
  session existed and both hosts were up, so the `Unreachable` placeholder path
  never engaged.
- **Per-frame allocation counts are static reads**, not an allocation profile;
  no allocator instrumentation was added. The O(sessions) left-panel rebuild is
  described by code inspection, not by a measured allocation count.

---

## Quick reference

| I want to… | Do this |
| --- | --- |
| Measure startup | `THURBOX_PERF_LOG=1 thurbox`, read the `startup` line in `thurbox.log` |
| Break down startup time | Read the `startup` line's phase fields: `config_init_ms`, `db_open_ms`, `theme_activate_ms`, `extension_heal_ms`, `heartbeat_ms`, `ui_build_ms` (building the Lua interface) and `first_frame_ms` |
| Watch steady-state cost | `THURBOX_PERF_LOG=1 thurbox`, read the `perf_window` lines (~1000 iterations: counter deltas + frame/republish/tick percentiles + slow ops) |
| Attribute an interactive stall | Look for `slow op` warnings in `thurbox.log` (named op + ms), or the slow-op list in `perf_window` |
| Watch perf live in the TUI | Press `F12` (perf HUD overlay; `[features] perf_hud`) |
| Inspect a running TUI from outside | `thurbox-cli perf` (needs THURBOX_PERF_LOG or an open HUD in that TUI) |
| See what a frame costs | `thurbox-cli perf` — `frame` is the paint and `republish` the table rebuild beside it; a frame is roughly the two added together |
| See binary size | Check the `Binary Size` CI job summary, or `cargo bloat --release --crates` |
| Profile CPU | `cargo flamegraph --profile release-with-debug --bin thurbox` |
| Verify no perf regression | `cargo nextest run -E 'test(kernel::perf)'` for the counters; the loop's settling is asserted per surface in `tests/*.rs` |
| Confirm idle CPU is low | `scripts/dev/perf-run.sh --idle` — or launch and leave it idle, where `idle skips` climbs while `frames` stays flat |
| Measure CPU under a real load | `scripts/dev/perf-run.sh -n 19 -p 3 -s 255x62` (see **Measuring**, below) |
| See where the time in a frame goes | `cargo bench --bench frame_cost` |
| Attribute a change | Run one of the two above before and after — a paired reading at the same size and session count, never two absolute numbers from different days |
