## 1. Declaration and trust

- [x] 1.1 Parse `capabilities = { … }` off a plugin's declaration in
      `kernel::host`, carry it on `Plugin`, and reject an unknown capability name
      at load with the file named
- [x] 1.2 Persist trust beside the interface's other user decisions (`ui.json`):
      absolute path → the digest trusted at the time; grant, revoke and read
- [x] 1.3 Surface on `kernel::inventory::Row` whether a file asks to run programs,
      whether it is trusted, and whether its contents have drifted since trusting
- [x] 1.4 Show all three in the settings modal's Interface tab, and add trust /
      revoke there beside `restore` and `remove`
- [x] 1.5 Revoking triggers the same reload an edit does, so the capability is
      gone on the next frame rather than refusing at call time (design D7)
- [x] 1.6 Tests: an untrusted plugin is refused; trusting one does not trust
      another; revoking takes effect without a restart; a drifted trusted file is
      reported as such

## 2. The run store

- [x] 2.1 New `src/kernel/runs.rs`: `RunStore` with `request(plugin, key, program,
      session, opts)` / `poll()` / `get(plugin, key)`, modelled on `kernel::diff`
- [x] 2.2 `Run` result type — stdout, stderr, exit status, truncated flag,
      completion instant — with `Pending` distinguishable from "finished empty"
- [x] 2.3 Freshness: a TTL per ask with a kernel default and floor, plus an
      explicit `refresh`; asking inside the TTL is a no-op that keeps the old
      result readable
- [x] 2.4 Bounded worker pool with a FIFO queue, so asks beyond the bound are
      queued rather than dropped
- [x] 2.5 Output cap per stream with the truncation flagged, and a wall-clock
      timeout that terminates the child and reports the timeout
- [x] 2.6 Drop a plugin's results when it unloads, so a reload cannot accumulate
      them
- [x] 2.7 Unit tests: freshness (no re-run inside the TTL, re-run after it,
      refresh overrides), queueing past the bound, truncation flagged, timeout
      reported, results dropped on unload

## 3. Running it in the right place

- [x] 3.1 Resolve the session's host through `session_ops::resolve_host` **before**
      the worker starts; an unresolvable host fails the run naming the host and
      runs nothing locally
- [x] 3.2 Local runs through the same path `AutomationAction::Exec` uses; remote
      runs through `git::host_shell_c` in the session's working directory on that
      host
- [x] 3.3 A run naming a session that is not present fails with a reason
- [x] 3.4 Tests: a remote session with a missing host is refused rather than run
      locally; a missing session is refused; the working directory is the
      session's

## 4. The Lua surface

- [x] 4.1 Install `run` on the command bus only for a plugin that declared it
      **and** is trusted — not installed, rather than installed and refusing
      (design D7)
- [x] 4.2 Publish `thurbox.runs` keyed by the asking plugin's own keys, so a
      plugin reads back what it asked for and cannot see another's
- [x] 4.3 Declare `run` and `thurbox.runs` in `thurbox.yml` so selene and
      lua-language-server see the real shape
- [x] 4.4 Tests: an undeclared plugin has no `run`; a declared but untrusted one
      has no `run`; two plugins' keys do not collide; several outstanding runs are
      independently readable and a finished one draws while another is pending

## 5. Modularity

- [x] 5.1 Hold the multi-module guarantee with a test: a user plugin under its own
      subdirectory loading its own modules, and a module path that escapes the
      interface directory being refused
- [ ] 5.2 `thurbox-cli plugin check` reports a failure in any module of a
      multi-module plugin, naming the file
- [ ] 5.3 `thurbox-cli plugin check` reports an unknown declared capability
      (non-zero) and an untrusted one (a note, not a failure)

## 6. The worked example

- [x] 6.1 Write `docs/examples/composite.lua` (or a directory, if it wants to be
      several modules): a pane over at least two programs, parsing their output,
      with an aligned table whose rows carry state
- [ ] 6.2 If the table cannot be expressed without re-deriving column arithmetic
      in the plugin, add the missing primitive to `ui/lib/widgets.lua` — this
      resolves design D8's open question
- [x] 6.3 Test that the example loads the way the interface loads it, as
      `plugin.lua` already is
- [x] 6.4 Show in the example what a pane draws when it has not been trusted —
      the state every user meets first, so it must not look like a broken pane

## 7. Documentation

- [x] 7.1 `docs/PLUGINS.md`: the capability, how to declare it, how a user trusts
      a plugin with it, the bounds, and the "ask every frame" pattern
- [x] 7.2 `docs/PLUGINS.md` Traps: a run is not a stream (the shell pane is), and
      a plugin must handle not being trusted — the capability is absent, so
      `command("run", …)` is a nil call, not a refusal
- [x] 7.3 `CLAUDE.md`: the eighth worker, and the security position — the first
      capability that reaches outside thurbox, granted per plugin by trust, and
      explicitly not a sandbox
- [x] 7.4 `docs/V2-KERNEL.md`: how "capabilities are absent rather than blocked"
      survives a capability that is conditionally installed

## 8. Verification

- [ ] 8.1 `just lint` clean, including selene and lua-language-server over the new
      example and `thurbox.yml`
- [ ] 8.2 Full suite green; architecture rules unchanged (`kernel` gains no new
      module it may not reach)
- [ ] 8.3 Exercise it by hand in the sandbox (`scripts/dev/sandbox.sh --v2
      --fresh`): the example pane against a real repository, and against a remote
      host if one is configured
