---
name: thurbox-testing
description: How thurbox is tested: the kernel/interface test files and what each one pins (frames, render proptests, tui_e2e on a real pty), the GIT_* scrub rule for tests that shell out to git, and the session-backend e2e harnesses under scripts/dev/e2e. Use when writing, running, debugging or extending thurbox tests, when a test shells out to git, or when working on the linux-container / windows-vm / real-host e2e scripts.
---

# Testing thurbox

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
bats scripts/install.bats             # Test install script (requires bats-core)
bats extensions/*/scripts/*.bats      # Test the extensions' shell scripts
just test-scripts                     # Both of the above, the way CI runs them
```

### Kernel and interface tests

The interface is Lua on a Rust kernel, so most coverage drives the **real
kernel over the real `ui/`** rather than a harness that imitates either:

- **`tests/kernel_mvp.rs`** — the kernel's contract: the four node kinds and their
  count, the plugin environment enumerated global-by-global (no blanket exemption
  for a leading underscore — that is how a capability once hid under `__run_impl`),
  the instruction/memory bounds, snapshot reads, and painting a plugin to a
  `TestBackend`.
- **The per-surface files** — one file per surface or contract:
  `session_list`, `search`, `new_session`, `terminal_pane`, `session_lifetime`,
  `keymap`, `focus`, `modals`, `chrome`, `mouse`, `hover`, `decoration`,
  `plugin_{authoring,commands,lifecycle,settings,switching}`, `repo_memory`,
  `remote_status`, `session_status`, `core_settings`, `attach_by_name`.
  Several build an interface in a tempdir from the embedded copy, so delivery and
  loading are exercised together.
- **`tests/kernel_limits.rs`** — instruction and memory ceilings, in their own file
  because they mutate process-wide limits.
- **Lua statics** — `selene ui` (undefined names + the sandbox, via `thurbox.yml`),
  `lua-language-server --check` (types + withheld libraries), `stylua` (format).
  The three cover different halves; see **Linting & Formatting**.
- **`tests/frames.rs`** — the bundled panes' frames pinned cell for cell, as
  literals in the file (no snapshot tool): the session list grouped, nested,
  windowed, narrow and under double-width names; the selection as a *style*;
  the agent pane empty, detached, failed, and with a real vt100 screen behind
  its surface. A failing test prints the new frame as a literal to paste. Every
  input is pinned (the `default` preset by name, a fixed `elapsed`, a fixed
  snapshot) — keep it that way; a frame that moves on its own is worse than none.
- **`tests/render_props.rs`** — proptest crash invariants: every bundled pane
  renders and paints at any size down to one cell, the arrangement places its
  slots inside the screen and apart, no key sequence makes a pane throw (the
  creation flow included), and selection extraction survives arbitrary buffers
  and arbitrary vt100 byte streams.
- **`tests/tui_e2e.rs`** (unix) — the real binary on a real pty, via `libc`'s
  `openpty` (no PTY crate), fully isolated (private HOME/config/data, a short
  private `TMUX_TMPDIR`, network and heartbeat features off). It asserts what no
  `TestBackend` test can: the boot frame, the kernel overlays opening and closing,
  the search strip taking focus, a column toggle reflowing with no screen clear,
  a resize storm down to 1×1, a broken pane reported through the Interface tab,
  exit restoring the terminal (alternate screen, mouse, bracketed paste, cursor)
  — and, where tmux exists, a headlessly created session attached, painted and
  typed into (`sh` as the agent). Also `just smoke`. It replaced the bash tmux
  smoke script, which could not see the byte stream and duplicated this harness.

Tests that shell out to `git` **must scrub the `GIT_*` location variables**
(`git::GIT_LOCATION_ENV`): git exports them to hook processes, so the suite running
under this project's own pre-commit `cargo nextest` inherits a `GIT_DIR` pointing at
the real repository. `tests/repo_memory.rs` and `tests/create_e2e.rs` show the
shape.

A test that creates a directory **owns its removal**: make it with
`tempfile::TempDir` (a dev-dependency) and hold the handle for as long as the test
needs it — never a bare path under `std::env::temp_dir()`. That temp dir is tmpfs on
many machines, so anything left there is leaked RAM until reboot, and nextest's
process-per-test multiplies one leak by the size of the suite. `paths`' `cfg(test)`
config/data sandbox is the sole exception, because it cannot be owned: a test's
`TestPathGuard` is thread-local, so work the test fans out to threads resolves paths
through the sandbox instead, which therefore has to outlive every thread in the
process. A `static` holds it and `atexit` removes it.
`paths::tests::no_unit_test_temp_dir_outlives_the_test_process` guards the rule for
both by re-running the tests that create them in a child process and checking what
survived it.

> v1's in-process acceptance harness, its `insta` snapshots, its invariant monkey
> test and `tests/v1_recordings.rs` were deleted with `src/app`. They are in the
> history if a behaviour needs archaeology.

### Session-backend e2e harnesses

One family under `scripts/dev/e2e/` (`linux-container.sh` = ephemeral Podman,
`windows-vm.sh` = ephemeral dockur Windows VM, `real-host.sh` = a machine you own)
sharing `e2e/lib/e2e-common.sh` — colour logging, the PASS/FAIL contract
(`E2E_JSON=1` for a machine-readable line), an in-shell `json_field` extractor (no
`python3`), the `[[hosts]]` emitter, and the `session create → get → assert` core.
`scripts/dev/README.md` is the newcomer index and carries the old→new path map.

