---
name: thurbox-testing
description: How thurbox is tested: the kernel/interface test files and what each one pins (frames, render proptests, tui_e2e on a real pty), the GIT_* scrub rule for tests that shell out to git, and the session-backend e2e harnesses under scripts/dev/e2e. Use when writing, running, debugging or extending thurbox tests, when a test shells out to git, or when working on the linux-container / windows-vm / real-host e2e scripts.
---

# Testing thurbox

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
bats scripts/install.bats             # Test install script (requires bats-core)
just test-scripts                     # Every bats suite (the justfile lists them)
```

The Nix development shell includes `bashInteractive`, not merely stdenv's
non-interactive Bash. Real-tmux fixtures exercise Readline's bracketed-paste
mode, including its `bind` setting, so substituting the stdenv shell changes
the terminal contract those tests are meant to drive.

`tests/doc_references.rs` keeps the tracked documents honest about each other: every
relative Markdown link, every `docs/<NAME>.md` and `.agents/skills/<name>` path
and every `github.com/Thurbeen/thurbox/blob/main/…` link names a file that
exists, and the names in its `RETIRED` table appear nowhere. Rename a document
or delete a symbol that docs cite, and that test lists every stale citation.
Add a name to `RETIRED` once nothing in the tree mentions it any more.

### Kernel and interface tests

The interface is Lua on a Rust kernel, so most coverage drives the **real
kernel over the real `ui/`** rather than a harness that imitates either:

- **`tests/kernel_mvp.rs`** — the kernel's contract: the four node kinds and their
  count, the plugin environment enumerated global-by-global (no blanket exemption
  for a leading underscore — that is how a capability once hid under `__run_impl`),
  the instruction/memory bounds, snapshot reads, and painting a plugin to a
  `TestBackend`.
- **`tests/fleet_engine_e2e.rs`** — the Fleet engine against
  the real `fleet` binary, found beside `thurbox-cli` (or `FLEET_BIN`), with
  Fleet's fake runtime: Tasks as rows through `fleet watch`, create, send,
  delete and confirmed delete through its command line, and the `fleet up`
  thurbox keeps running; and, on `runtime-mux` with tmux, a Task's pane through
  `fleet attach`. Skips when a binary is not built (`cargo build -p fleet-cli
  -p runtime-mux`), except under `CI`, where a missing binary fails the test:
  the Nextest job builds `fleet` before its run, since it excludes Fleet's
  crates.
- **The per-surface files** — one file per surface or contract:
  `session_list`, `search`, `new_session`, `terminal_pane`,
  `keymap`, `focus`, `modals`, `chrome`, `mouse`, `hover`, `decoration`,
  `plugin_{authoring,commands,lifecycle,settings,switching}`, `core_settings`.
  Several build an interface in a tempdir from the embedded copy, so delivery and
  loading are exercised together.
  `plugin_switching` also runs the registry persistence path in an isolated child
  process under a Unix file-size limit: the forced mid-write failure must preserve
  every byte of the prior `plugin-state.json` and `ui.json` and leave no staged
  file behind. The native
  interruption probe is Unix-only; on Windows the replacement is `std::fs::rename`.
  The same file pins config-file
  symlink preservation on Unix and paths beyond `MAX_PATH` on Windows.
- **`tests/edited_interface.rs`** — an edited interface keeps working across an
  upgrade. It holds files frozen from old releases with a user's edit
  (`tests/fixtures/edited_interface/<release>/`: v2.22.4's layout, session
  list and agent pane), and checks that they are preserved, arrange, render and pass `plugin check`
  against the current `lib/` and kernel. It also pins every name `lib/` exports
  (`lib_surface.txt`). This is the check behind `lib/`'s compatibility promise
  in `ui/AGENTS.md`. Never refresh the frozen files to follow `ui/`. A new
  `lib/` export is appended to the pin; a removed one means adding a shim back.
- **`tests/kernel_limits.rs`** — instruction and memory ceilings, in their own file
  because they mutate process-wide limits.
- **Lua statics** — `selene ui` (undefined names + the sandbox, via `thurbox.yml`),
  `lua-language-server --check` (types + withheld libraries, via
  `ui/lib/thurbox.d.lua`), `stylua` (format). The three cover different halves;
  see *Linting & Formatting* in `AGENTS.md`.
- **`scripts/ci/check-lua-types.sh`** — the definitions' own test. Three panes in
  `tests/fixtures/lua_types/` each misspell one thing the plugin API otherwise
  drops in silence (a node prop, a command option, a theme role) and each must
  still be reported. `--check ui` proves the panes are clean; this proves the
  types have teeth. Runs in the Lua Lint job and in `just lint`.
- **`scripts/ci/check-lua-std.sh`** — the same trick for `thurbox.yml`.
  `tests/fixtures/lua_std/reads.lua`, generated from `src/kernel/lua_api.rs`
  with the `thurbox.*` entries it lints against, reads every declared path and
  must lint clean, and `typos/` holds one pane per table misspelling one field,
  each of which must not. It asserts on selene's `incorrect_standard_library_use`
  code from its `Json2` output and attributes a finding by the file it came
  from, so no assertion depends on message wording. A checked path stops at the
  first `[…]`, so a list has nothing below it to probe, and `runs` is the one map
  whose keys are the plugin's own literals rather than a session id. Same two
  runners.
- **`tests/lua_api.rs`** — the plugin API against `kernel::lua_api`: the generated declaration files equal what it renders
  (`THURBOX_REGENERATE=1`, i.e. `just lua-api`, rewrites them), a pane walking
  every key of a fully populated publish finds none undeclared, and
  `thurbox.api`/`requires_api` hold; its `meta` module drives `ui.*` session
  meta from the real `thurbox-cli` to the bundled session list.
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
  private `TMUX_TMPDIR`, inherited `INPUTRC` scrubbed, network and heartbeat
  features off). It asserts what no `TestBackend` test can: the boot frame, the
  kernel overlays opening and closing,
  the search strip taking focus, a column toggle reflowing with no screen clear,
  a resize storm down to 1×1, a broken pane reported through the Interface tab,
  exit restoring the terminal (alternate screen, mouse, bracketed paste, cursor)
  — and, where tmux and a workspace build of `fleet` and `runtime-mux`
  exist, a Fleet session attached, painted and typed into. Under `CI` a missing
  `fleet` or `runtime-mux` fails these scenarios instead of skipping them.
  Every profile puts a stand-in `fleet` first on `PATH` (no Tasks, no
  manager), so no interface under test ever reaches a real Fleet store; `Profile::real_fleet` swaps in
  the real binary on a store, config and tmux directory of the profile's own,
  with `claude` running `sh`, and `Profile::fleet_session` applies and
  reconciles an interactive Task. `just smoke` drives the same harness.
  Keyboard negotiation covers responsive legacy, kitty and silent terminals;
  `docs/thurbox/development.md` records the reply invariant and execution measurements.
- **`tests/self_update_scratch.rs`** (unix) — the real update CLI with private
  config/data/home and bounded fake downloaders, terminated in its own process
  group mid-download. The leader stays unreaped until group teardown so the
  harness cannot signal a reused pid. Pins recovery on the next update while
  preserving an aged live owner's staging; no network or installed binary is
  touched. A subprocess probe also pins that the system temp fallback never
  reserves or sweeps unrelated paths. `update::self_update::tests` covers the same
  staging lifecycle in child processes on Unix and native Windows, including normal
  exit, recent work, legacy names, malformed ownership, and symlinks / Windows junctions.
- **`tests/spawn_with_failing_tmux_hook.rs`** (unix) — its own binary and its own
  socket because it installs a **server-global** hook, which any suite sharing
  the socket would then spawn into. Against a *real* tmux (skipped when tmux is
  absent): a dead `after-new-window` hook, the shape an uninstalled tmux plugin
  leaves behind. Pins that the one-shot `new-window` path believes the pane id
  on stdout rather than the exit status — tmux hands the client the status of
  the last `run-shell` its command list triggered, hooks included, so a window
  that was created came back as `exit status: 127` and was torn down.
  The second test pins the quieter half: the id kept must still resolve to that
  window and carry its session stamp.
- **`tests/path_resolution_absolute.rs`** (unix) — its own test binary because it
  moves the process working directory, which a sibling test in the same binary
  would see. Pins that `resolve_on_path` (`docs/thurbox/config.md` → How `command` is
  resolved) skips an empty `PATH` component rather than joining it as "the
  current directory", which would answer with a bare relative name.

Tests that shell out to `git` **must scrub the `GIT_*` location variables**
(`git::GIT_LOCATION_ENV`): git exports them to hook processes, so the suite running
under this project's own pre-commit `cargo nextest` inherits a `GIT_DIR` pointing at
the real repository. `tests/tui_e2e.rs`'s `git` helper shows the shape.

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

A test that starts a **tmux server does not own its removal** — a guard does.
`crates/runtime-mux/tests/support/tmux_server.rs`'s `TmuxServer` is that guard, every harness in
`tests/` holds one, and `tests/tmux_server_leak.rs` is the worked example and
the gate. Build one with `TmuxServer::pin(SOCKET)` (process-wide, which nextest
makes safe) or `TmuxServer::private(SOCKET)` plus `server.scope(&mut cmd)` per
child command, **hold it**, and it does three things no call site has to
remember:

1. **Pins the socket** (`runtime_mux::instance::SOCKET_OVERRIDE_ENV`), so teardown has a
   name to kill.
2. **Clears `SOCKET_OWNER_ENV`.** thurbox injects `THURBOX_SOCKET` *and*
   `THURBOX_SOCKET_FOR` into every pane it spawns, so a suite run inside a
   thurbox session inherits both. `runtime_mux::instance::socket_for` drops an override
   tagged for another instance's data dir — correctly: a harness that isolated
   its data directory but not its server would be spawning windows on the operator's
   tmux. So a harness that relocates `THURBOX_DATA_DIR` and leaves the tag in
   place lands on a *derived* socket, and its `kill-server` kills a name nothing
   created. One orphan server, agents and all, per run.
3. **Points `TMUX_TMPDIR` at a directory of its own** — the guard's own, not the
   harness's tempdir. tmux never unlinks a socket, so even a server killed
   correctly leaves a dead socket file behind; and owning the directory is what
   lets `Drop` kill the server *before* the socket goes, rather than after.

`Drop` is the point: a panic, a failed `.expect()` or a nextest
`slow-timeout` termination still runs it. A signal runs no destructor, which is
what `just reap-tmux` (`scripts/dev/reap-tmux-servers.sh`, Linux) sweeps up.

Two tests hold the line, both in `tests/tmux_server_leak.rs`:
`the_guard_scopes_every_socket_it_pins` reads (1)–(3) off the guard's own
source, and `no_harness_pins_a_socket_outside_the_guard` reads every file in
`tests/` and fails any that pins a socket by hand or builds a guard without
binding it. That second one checks each **site**, not each file —
`attach_by_name` scopes five times, and a file-wide check would let a sixth test
that pinned its own socket sit behind the other five. Both strip comments first,
so prose about the owner tag does not satisfy the rule; both are read off the
sources because a run that leaks still passes every assertion it makes.
`a_panicking_harness_still_reaps_its_tmux_server` is the behavioural half: it
runs a child copy of the test binary that starts a server and panics, then asks
the **process table** — not tmux, which cannot answer for a socketless server —
what survived.

`crates/runtime-mux/src/tmux_compat/control_mode/tests.rs`'s `ThrowawayServer` is the one harness that
cannot have a socket directory of its own — the lib's unit tests share a process
and `TMUX_TMPDIR` is process-wide — so it removes the socket file by hand
instead.

### Session-backend e2e harnesses

One family under `scripts/dev/e2e/` (`linux-container.sh` = ephemeral Podman,
`windows-vm.sh` = ephemeral dockur Windows VM, `real-host.sh` = a machine you own)
sharing `e2e/lib/e2e-common.sh` — colour logging, the PASS/FAIL contract
(`E2E_JSON=1` for a machine-readable line) and the `[[hosts]]` emitter.
`scripts/dev/README.md` is the newcomer index.

`windows-vm.sh test` checks runtime-mux's psmux hook-status gate (the
`HOOK_STATUS` constant in `crates/runtime-mux/src/psmux.rs`) against psmux
itself, and **fails** the harness when the two disagree. The verdict is about
the **mailbox** only: a holding pair says the gate may be reconsidered rather
than opened. A half counts as present only when the option comes back on the
pane it was set on and on **no other**, and the probe splits a second pane so
that difference is observable.
`scripts/dev/e2e/windows-vm.bats` drives those helpers — the gate read, each
half's measurement including the leak case, the verdict table and that a failed
probe reaches the exit status — with no VM to provision, so the failing branch
is covered in CI and by `just test-scripts`.

