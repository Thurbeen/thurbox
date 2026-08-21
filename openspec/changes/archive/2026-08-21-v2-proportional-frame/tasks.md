## 1. Change-signals

- [x] 1.1 Add a generation to `SnapshotStore`, bumped inside every path that
      replaces or edits the snapshot (`refresh`, hook states, output
      quiescence, acknowledge), and expose it as a read.
- [x] 1.2 Add a version to `Themes`, bumped when a palette is activated or the
      set is reloaded.
- [x] 1.3 Add a version to `Registry`, bumped when a setting, a binding, the
      disabled set or the collected declarations change.
- [x] 1.4 Add a version to `Terminals`' agent metadata, bumped in `meta()` only
      when it actually writes an entry (it is mutated on read today).
- [x] 1.5 Add versions for the worker-backed stores `publish` reads — diffs,
      links, content, metrics, repos, runs, inventory. Implemented as one
      app-side `data_epoch` fed by the `changed` flag each store's `poll`
      already returns, rather than a counter inside each store: a signal derived
      from the existing return value cannot drift from it. Links and the content
      scan compare before storing, so a re-scan that found the same answer does
      not move it.
- [x] 1.6 Test: mutating each source moves the epoch, and doing nothing leaves
      it still. This is the test that catches the one failure that is otherwise
      silent (design.md — Risks).

## 2. Gated publish

- [x] 2.1 Introduce the publish epoch: the versions from task 1 taken together,
      plus the small scalars compared directly (focus, hovered, status).
- [x] 2.2 Split `publish` into per-group builders, each naming the versions it
      is built from, keeping the outer `thurbox` table assembled fresh each
      frame from either a rebuilt group or last frame's.
- [x] 2.3 Rebuild a group only when one of its versions moved.
- [x] 2.4 Test: for each source, mutate it and assert a gated publish produces
      exactly what a full rebuild produces — the proof named in design.md.
- [x] 2.5 Test: with nothing mutated, a second publish rebuilds no group.
- [x] 2.6 Measure: paired before/after under the ADR-P15 load, recording CPU per
      frame as well as total CPU. Result: 9100us -> 6862us per frame (-25%),
      31.0% -> 28.4% CPU, 34.1 -> 41.4 fps.

## 3. The purity declaration

- [x] 3.1 Read `pure` from a plugin's declaration table, defaulting to absent,
      and carry it on the loaded plugin beside `slot` and `focusable`.
- [x] 3.2 Add `pure` to `thurbox.yml` so the key lints — **not done, premise was
      wrong**: `thurbox.yml` is selene's stdlib for the `thurbox` *global*, not a
      schema for the table a plugin returns, and no declaration key (`slot`,
      `focusable`, `order`) is checked today. Reporting a misspelled `pure` would
      mean inventing a schema for the whole declaration table, which is a
      separate change. The spec scenario is amended to what holds: a misspelling
      leaves the pane rendering every frame, which is the safe direction and the
      reason the declaration is opt-in.
- [x] 3.3 Cache a pure pane's converted tree under `(epoch, width, height,
      focused)`; on a hit, skip both the Lua call and the conversion and reuse
      the cached tree.
- [x] 3.4 Drop the whole cache whenever the host is rebuilt, so no tree outlives
      a reload.
- [x] 3.5 Keep the surface-moved path intact: a reused tree still paints when a
      terminal surface under it has produced output.
- [x] 3.6 Test: a pure pane is not re-rendered while its inputs are unchanged;
      it is re-rendered when the epoch moves, when its rect changes, when focus
      changes, and after a reload.
- [x] 3.7 Test: an undeclared pane renders every frame, unchanged.

## 4. Declare the bundled panes

- [x] 4.1 Audit each bundled pane's render for shared-state writes and clock
      reads; record which qualify.
- [x] 4.2 Declare `pure` on the panes that qualify (`10_sessions` and
      `20_agent` are the measured wins). The audit found `10_sessions` reads
      `ctx.elapsed` for the working spinner, which the original contract
      disqualified; the tree key now carries the spinner's own 8Hz tick
      (`ANIMATION_HZ`) so it stays pure and still animates. The suite then found
      a second hole: a pure render may read `store`/`state`, which handlers
      write, so the key carries a `StateVersion` too.
- [x] 4.3 Leave `65_search` undeclared, with a comment naming the `store` writes
      in its render as the reason.

## 5. Observability

- [x] 5.1 Count skipped publishes and skipped renders in `kernel::perf`.
- [x] 5.2 Render both in the perf HUD and `thurbox-cli perf`, and extend the
      producer/renderer drift test in `tests/kernel_perf.rs`.
- [x] 5.3 Verify the decision costs no wall-clock read when timing is off — the
      gate is an integer compare on both paths and reads no clock at all, so it
      is unconditional rather than gated on timing.

## 6. Documentation

- [x] 6.1 `docs/PLUGINS.md`: describe `pure`, what declaring it asserts, and the
      two disqualifiers — under Traps, since the failure is invisible at load.
- [x] 6.2 `docs/V2-KERNEL.md`: a pane's render is no longer guaranteed once per
      frame.
- [x] 6.3 `docs/PERFORMANCE.md`: close out ADR-P15's "still open" and record the
      measured result.
- [x] 6.4 `CLAUDE.md`: the publish and render rules in the performance section.

## 7. Land

- [x] 7.1 Full gates: `cargo fmt`, clippy `-D warnings`, `cargo nextest run
      --all` (1913 passed), `RUSTDOCFLAGS="-D warnings" cargo doc`, `rumdl` — all
      clean. **`selene`, `stylua` and `lua-language-server` are not installed on
      this machine** (they come from the Nix flake, which is also absent) and did
      NOT run. The Lua change is one `pure = true,` assignment plus comments in
      three files, all <=82 columns at the matching 2-space indent, introducing
      no identifiers or globals — but the three gates are unverified and CI is
      the first thing that will actually run them.
- [x] 7.2 Final paired measurement against v1 under the ADR-P15 load, idle and
      loaded. Loaded: v1 15.6% / 3139us per frame, v2 23.1% / 5175us — 1.48x on
      CPU, down from 2.48x. Idle: v1 2.43%, v2 5.03%.
