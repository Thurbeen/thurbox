# Performance

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
stays stale longer than a blink. The black-box smoke test
(`scripts/dev/smoke/tui-smoke.sh`) still asserts the first frame and every
post-keystroke frame paint.

**One pass rides along after each paint**:
`App::paint_terminal_hyperlinks` re-emits the frame's OSC 8 hyperlinks so the
outer terminal can offer its own open-link gesture (see the Clickable URLs
section of `docs/FEATURES.md`). It is bound to the *painted* frames — ratatui
rewrites the cells, so the escapes must follow each draw — and is gated on
`HyperlinkTable::is_empty()` **before** it computes layout or extracts screen
rows, so a session whose agent never printed a link pays a single emptiness
check per frame. When links are present the scan is bounded (the newest 128
runs × the visible rows) and the writes are a handful of short `queue!`s per
visible run.

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
`Ctrl+P` parent scan — each run on a `BackgroundTask` worker drained by
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
| Break down startup time | Read the `startup` phase fields (`config_init_ms`/`db_open_ms`/`theme_activate_ms`/`extension_heal_ms`/`app_new_ms`/`restore_ms`/`heartbeat_ms`) + the `restore_discover`/`restore_adopt` lines |
| Watch steady-state cost | `THURBOX_PERF_LOG=1 thurbox`, read the `perf_window` lines (~10 s cadence: counter deltas + frame/tick percentiles + slow ops) |
| Attribute an interactive stall | Look for `slow op` warnings in `thurbox.log` (named op + ms), or the slow-op list in `perf_window` |
| Watch perf live in the TUI | Press `F12` (perf HUD overlay; `[features] perf_hud`) |
| Inspect a running TUI from outside | `thurbox-cli perf` (needs THURBOX_PERF_LOG or an open HUD in that TUI) |
| Verify the status-hook cache (ADR-P6) | `cargo nextest run -E 'test(perf_hook_states)'`; `hook_state_loads` stays flat while idle, +1 per external `session signal` |
| See binary size | Check the `Binary Size` CI job summary, or `cargo bloat --release --crates` |
| Profile CPU | `cargo flamegraph --profile release-with-debug --bin thurbox` |
| Verify no perf regression | `cargo nextest run -E 'test(perf_)'` |
| Confirm idle CPU is low | Launch, leave it idle — `redraws_skipped` climbs while `frames_rendered` stays flat |
