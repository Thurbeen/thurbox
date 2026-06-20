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
(`scripts/dev/tui-smoke-test.sh`) still asserts the first frame and every
post-keystroke frame paint.

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
  `first_frame_ms=…` line (covering config load, DB open, and session restore)
  to `~/.local/share/thurbox/thurbox.log`. Off by default — never affects normal
  runs or the smoke test.
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

## Quick reference

| I want to… | Do this |
| --- | --- |
| Measure startup | `THURBOX_PERF_LOG=1 thurbox`, read `first_frame_ms` in `thurbox.log` |
| See binary size | Check the `Binary Size` CI job summary, or `cargo bloat --release --crates` |
| Profile CPU | `cargo flamegraph --profile release-with-debug --bin thurbox` |
| Verify no perf regression | `cargo nextest run -E 'test(perf_)'` |
| Confirm idle CPU is low | Launch, leave it idle — `redraws_skipped` climbs while `frames_rendered` stays flat |
