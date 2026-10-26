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
bats extensions/*/scripts/*.bats      # Test the extensions' shell scripts
just test-scripts                     # Both of the above, the way CI runs them
```

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
- **The per-surface files** — one file per surface or contract:
  `session_list`, `search`, `new_session`, `terminal_pane`, `session_lifetime`,
  `keymap`, `focus`, `modals`, `chrome`, `mouse`, `hover`, `decoration`,
  `plugin_{authoring,commands,lifecycle,settings,switching}`, `repo_memory`,
  `remote_status`, `session_status`, `core_settings`, `attach_by_name`.
  Several build an interface in a tempdir from the embedded copy, so delivery and
  loading are exercised together.
  `plugin_switching` also runs the registry persistence path in an isolated child
  process under a Unix file-size limit: the forced mid-write failure must preserve
  every byte of the prior `ui.json` and leave no staged file behind. The native
  interruption probe is Unix-only; Windows replacement is compile-checked and uses
  `MoveFileExW`, but is not exercised by that probe.
- **`tests/edited_interface.rs`** — an edited interface keeps working across an
  upgrade. It holds files frozen from old releases with a user's edit
  (`tests/fixtures/edited_interface/<release>/`: v2.22.4's layout, session
  list and agent pane, v2.32.0's layout and agent pane), and checks that they are preserved, arrange, render and pass `plugin check`
  against the current `lib/` and kernel. It also pins every name `lib/` exports
  (`lib_surface.txt`). This is the check behind `lib/`'s compatibility promise
  in `ui/AGENTS.md`. Never refresh the frozen files to follow `ui/`. A new
  `lib/` export is appended to the pin; a removed one means adding a shim back.
- **`tests/kernel_limits.rs`** — instruction and memory ceilings, in their own file
  because they mutate process-wide limits.
- **Lua statics** — `selene ui` (undefined names + the sandbox, via `thurbox.yml`),
  `lua-language-server --check` (types + withheld libraries, via
  `ui/lib/thurbox.d.lua`), `stylua` (format). The three cover different halves;
  see **Linting & Formatting**.
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
  whose keys are the plugin's own literals rather than a session id. Those tables
  were declared without their fields for as long as nothing read them (issue
  #1133). Same two runners.
- **`tests/lua_api.rs`** — the plugin API against `kernel::lua_api` (issue
  #1476): the generated declaration files equal what it renders
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
  private `TMUX_TMPDIR`, network and heartbeat features off). It asserts what no
  `TestBackend` test can: the boot frame, the kernel overlays opening and closing,
  the search strip taking focus, a column toggle reflowing with no screen clear,
  a resize storm down to 1×1, a broken pane reported through the Interface tab,
  exit restoring the terminal (alternate screen, mouse, bracketed paste, cursor)
  — and, where tmux exists, a headlessly created session attached, painted and
  typed into (`sh` as the agent), the classic Shell tab raised, lowered and
  still holding its shell (and replaced by a working one after it exited
  behind the agent, or its window went while thurbox was closed), and a v2.32.0 layout-preset profile (fixture in
  `tests/fixtures/v2_32_0_split_shell/`) upgrading back to classic. Also
  delayed remote shell creation and re-adoption keeping F1 responsive, and
  `just smoke`. It replaced the bash tmux
  smoke script, which could not see the byte stream and duplicated this harness.
  Keyboard negotiation covers responsive legacy, kitty and silent terminals;
  `docs/DEVELOPMENT.md` records the reply invariant and execution measurements.
  Two scenarios there cover a **remote session whose link has gone bad**, which
  needs a stand-in that a local tmux cannot supply on its own: `tmux -C
  attach-session` hands its stdin and stdout *file descriptors* to the tmux
  server and then only shepherds, so stopping the client changes nothing, while
  a real `ssh` is a byte relay whose stopping wedges the link. The `ssh`
  stand-in therefore runs the control connection through a pair of `cat` pumps
  over fifos — the pids the test `SIGSTOP`s — and `exec`s straight through for
  every other call, whose **exit status is load-bearing** (`has-session`
  answering "no" is how `ensure_ready` decides to create a session). Both
  scenarios assert only on a kernel-owned overlay, never on the session list:
  the list returns on a snapshot tick, seconds even on a healthy link, so it
  cannot tell a frozen interface from a patient one. See ADR-P24.
- **`tests/self_update_scratch.rs`** (unix) — the real update CLI with private
  config/data/home and bounded fake downloaders, terminated in its own process
  group mid-download. The leader stays unreaped until group teardown so the
  harness cannot signal a reused pid. Pins recovery on the next update while
  preserving an aged live owner's staging; no network or installed binary is
  touched. A subprocess probe also pins that the system temp fallback never
  reserves or sweeps unrelated paths. `agent::self_update::tests` covers the same
  staging lifecycle in child processes on Unix and native Windows, including normal
  exit, recent work, legacy names, malformed ownership, and symlinks / Windows junctions.
- **`tests/reap_e2e.rs`** — window-teardown ownership against a *real* tmux on a
  throwaway socket (skipped when tmux is absent), because the bug it pins only
  exists in how tmux resolves a target. 15 tests. Seven pin the reap itself: a
  stale row's reap spares a live namesake's window (stamped, and unstamped as
  every psmux window is), a namesake's pane the stale
  row still remembers, a live window whose name only collides after
  `sanitize_window_name` ('fleet 1' and 'fleet_1' share one `tb-fleet_1`), and a
  soft-deleted namesake still inside its undo window — while a row whose own
  pane resolves, and one whose pane id resolves to nothing but whose stamp
  nobody else answers to, still lose their window. Sparing must not be bought by
  making the reap a no-op.
  Force delete, `stop` and `restart` share the reap's ownership gate (ADR-25,
  `backend::identity::WindowIndex`) rather than each resolving `tb-<name>` on their
  own, so one test walks all three against their own live namesake, plus one
  asserting force delete still kills the row's own window. Two more cover
  the stamp itself: a row with no pane id at all (the psmux shape) still
  resolves its own window, and a restore whose name a live session now answers
  to is refused outright (issue #1192) rather than joining it on the backend —
  `respawn`'s own ownership gate is walked by `restart` in the three-path test
  above. The last three cover the
  companion shell and the no-server guarantee (ADR-24/25's remote teardown):
  force delete and reap both collect the companion shell alongside the agent's
  window, a teardown spares a live namesake's companion shell, and a teardown
  never brings a tmux server into being (the `ensure_ready` side effect this
  path must not trigger). The owed-teardown sweep
  (`retry_owed_remote_teardowns`) is unit-tested against `host_cli::fake` in
  `session_ops::delete` — that a failed remote teardown is written onto the
  row, that a soft-deleted row is never on its worklist (the undo window), and
  that a silent host is asked once per pass — and proven end to end on a real
  process by `linux-container.sh`'s `remote_teardown_probe`. The same file
  covers a delegated delete whose host never answers at all: it falls back to
  the local teardown and owes a retry, while any *other* host error still
  aborts. `restart --if-missing` refusing to relaunch against an unreachable
  host (rather than reading it as "no window" and starting a second agent) is
  proven end to end by `linux-container.sh`'s `restart_if_missing_probe`.
- **`tests/concurrent_respawn.rs`** — six sessions relaunching onto a tmux
  server that does not exist yet, against a *real* tmux on a throwaway socket
  (skipped when tmux is absent): every worker's `ensure_session_configured`
  sees "no session" and races `new-session`, and the regression this pins is
  that a loser must not abort its whole respawn over tmux's `duplicate
  session` — it has to notice the winner's session and continue.
- **`tests/spawn_command_resolution.rs`** (unix) — a local spawn against a *real*
  tmux on a throwaway socket (skipped when tmux is absent): starts a server
  whose own `PATH` lacks the agent, then spawns it through `TmuxBackend`'s
  control-mode path with the agent only on thurbox's `PATH`. Pins the fix in
  `resolve_local_program` (`docs/CONFIG.md` → How `command` is resolved): a
  local window command used to be left for the multiplexer to resolve, which
  sees the *server's* `PATH` (an attached client's is not copied in), not the
  one thurbox itself was launched with.
- **`tests/spawn_with_failing_tmux_hook.rs`** (unix) — its own binary and its own
  socket because it installs a **server-global** hook, which any suite sharing
  the socket would then spawn into. Against a *real* tmux (skipped when tmux is
  absent): a dead `after-new-window` hook, the shape an uninstalled tmux plugin
  leaves behind. Pins that the one-shot `new-window` path believes the pane id
  on stdout rather than the exit status — tmux hands the client the status of
  the last `run-shell` its command list triggered, hooks included, so a window
  that was created came back as `exit status: 127` and was torn down (#1154).
  The second test pins the quieter half: the id kept must still resolve to that
  window and carry its session stamp.
- **`tests/path_resolution_absolute.rs`** (unix) — its own test binary because it
  moves the process working directory, which a sibling test in the same binary
  would see. Pins that `resolve_on_path` (`docs/CONFIG.md` → How `command` is
  resolved) skips an empty `PATH` component rather than joining it as "the
  current directory": that join used to answer with a bare relative name,
  reintroducing the multiplexer-resolves-it dependence the function exists to
  remove.
- **`tests/session_state_agreement.rs`** — one row, four independent readers
  (`session get`, `session list`, `watch --initial` via the real binary, and
  `SnapshotStore` in-process): all four must answer the same `SessionState`.
  Pins the concrete regression the `session::hook_status` consolidation fixed —
  `seen_at` is a stored fact only the snapshot used to read, so a turn the
  interface already showed as `idle` kept answering `done` on every headless
  surface for the rest of the session's life.

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

A test that starts a **tmux server does not own its removal** — a guard does.
`tests/support/tmux_server.rs`'s `TmuxServer` is that guard, every harness in
`tests/` holds one, and `tests/tmux_server_leak.rs` is the worked example and
the gate. Build one with `TmuxServer::pin(SOCKET)` (process-wide, which nextest
makes safe) or `TmuxServer::private(SOCKET)` plus `server.scope(&mut cmd)` per
child command, **hold it**, and it does three things no call site has to
remember:

1. **Pins the socket** (`backend::instance::SOCKET_OVERRIDE_ENV`), so teardown has a
   name to kill.
2. **Clears `SOCKET_OWNER_ENV`.** thurbox injects `THURBOX_SOCKET` *and*
   `THURBOX_SOCKET_FOR` into every pane it spawns, so a suite run inside a
   thurbox session inherits both. `backend::instance::socket_for` drops an override
   tagged for another instance's data dir — correctly: a harness that isolated
   its database but not its server would be spawning windows on the operator's
   tmux. So a harness that relocates `THURBOX_DATA_DIR` and leaves the tag in
   place lands on a *derived* socket, and its `kill-server` kills a name nothing
   created. One orphan server, agents and all, per run.
3. **Points `TMUX_TMPDIR` at a directory of its own** — the guard's own, not the
   harness's tempdir. tmux never unlinks a socket, so even a server killed
   correctly leaves a dead socket file behind; and owning the directory is what
   lets `Drop` kill the server *before* the socket goes, rather than after,
   which is the ordering the old shape got wrong.

`Drop` is the point. Teardown used to be a `cleanup()` call written at each exit
point — eleven of them in `spawn_command_resolution`, more in `create_e2e` — and
a panic, a failed `.expect()` or a nextest `slow-timeout` termination reached
none of them. The server survived with its socket file gone, so nothing could
connect to reap it: one machine held 400 orphans, 1432 processes and 4.4 GiB
RSS, with 4 sockets between 433 servers (issue #1175). A signal still runs no
destructor, which is what `just reap-tmux`
(`scripts/dev/reap-tmux-servers.sh`, Linux) sweeps up.

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

`src/backend/tmux_compat/control_mode/tests.rs`'s `ThrowawayServer` is the one harness that
cannot have a socket directory of its own — the lib's unit tests share a process
and `TMUX_TMPDIR` is process-wide — so it removes the socket file by hand
instead.

> v1's in-process acceptance harness, its `insta` snapshots, its invariant monkey
> test and `tests/v1_recordings.rs` were deleted with `src/app`. They are in the
> history if a behaviour needs archaeology.

### Session-backend e2e harnesses

`tests/create_e2e.rs` also kills a creator after its window exists and a force
deleter during git worktree removal, expires only the killed operation's leases
in its private database, and verifies the shared sweep removes the resources.
It pins failed-launch rollback, same-branch retry, stale-upsert protection and
purge tombstones. `tests/cli_automation_lifecycle.rs` covers owner and target
suspensions through real CLI calls, preserving ids, history and prior enabled
state, and missing-pane confirmation across separate heartbeat processes.

`tests/create_e2e.rs` checks multi-repo directory scope through the real spawn,
restart and restore pipelines using a recording agent. Checkout cleanup also
checks active cwd and attachment claims (including nested directories), with
creation/cleanup directory lease contention covered in the same suite. An opt-in
test also runs the installed Claude CLI against a loopback fake API: no model calls or real
credentials. It verifies member edits through workspace and canonical paths,
resume, and denial of a nested symlink to a non-member directory:

```bash
THURBOX_TEST_CLAUDE="$(command -v claude)" cargo nextest run --test create_e2e \
  --run-ignored all -E 'test(installed_claude_edits_members)'
```

This test requires Python 3 and Claude Code with `--bare` support. It is Unix
only; native Windows directory arguments are covered by the portable registry
tests, but this does not attest live psmux/rmux permission behavior.

On native Windows, `tests/session_lifetime.rs` runs
`scripts/dev/e2e/psmux-exit-control.ps1` when psmux is installed. It creates a
random socket and disposable config/data profile, opts out of agent wiring in
that fresh profile, and checks a real TUI's crash-loop message and failed-command
event after three actual CLI relaunches. Cleanup runs in `finally`. It runs
`scripts/dev/e2e/psmux-pane-routing.ps1` the same way (#1557): three sessions of
a fixture agent that logs what it reads, each sent its own line, then one
stopped; it passes when every line reached its own session and only that window
ended, or when thurbox refused a psmux older than 3.3.8. `-PsmuxDir` puts a
given psmux first on `PATH`, which is how it was run against 3.3.7, 3.3.8 and
3.4.0; `-ServerPsmuxDir` starts thurbox's server with an older psmux first, the
server an upgrade leaves running.

One family under `scripts/dev/e2e/` (`linux-container.sh` = ephemeral Podman,
`windows-vm.sh` = ephemeral dockur Windows VM, `real-host.sh` = a machine you own)
sharing `e2e/lib/e2e-common.sh` — colour logging, the PASS/FAIL contract
(`E2E_JSON=1` for a machine-readable line), an in-shell `json_field` extractor (no
`python3`), the `[[hosts]]` emitter, and the `session create → get → assert` core.
`scripts/dev/README.md` is the newcomer index and carries the old→new path map.

`windows-vm.sh test` additionally holds thurbox's psmux hook-status gate
(`Psmux::HOOK_STATUS`) against psmux itself: it asks the binary which way the
gate is set (`thurbox-cli runtime status --json` → `hook_status["local:psmux"]`,
never the source) and **fails** the harness when
the two disagree — a gate open over a mailbox psmux drops, or a psmux that has
grown the scope while the gate is still closed. Before that the probe reported
on its own `ok` branch whichever way the measurement went, which is why psmux
implementing no pane user options at all sat unseen (issue #1170).
The verdict is about the **mailbox**, not the whole gate: that also rests on
claude accepting the forward-slash `--settings` path on Windows, which needs a
real agent launch and is measured nowhere in the harness, so a holding pair
prints that condition beside itself and says the gate may be reconsidered
rather than opened. A half counts as present only when the option comes back on
the pane it was set on and on **no other** — the poller maps one pane to one session, so an option
stored at window or server scope would attribute one session's state to
another, and the probe splits a second pane so that difference is observable.
`scripts/dev/e2e/windows-vm.bats` drives those helpers — the gate read, each
half's measurement including the leak case, the verdict table and that a failed
probe reaches the exit status — with no VM to provision, so the failing branch
is covered in CI and by `just test-scripts`.

