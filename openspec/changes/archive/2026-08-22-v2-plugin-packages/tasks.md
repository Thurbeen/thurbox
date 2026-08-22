## 1. The unplaced diagnosis (independent of packaging; ship first)

- [x] 1.1 Add a pure function in `kernel::layout` (or a helper beside `resolve`) returning the set of slots an arrangement places at a given size, so `check` can compare it against `LuaHost::occupied_slots()` without painting a frame
- [x] 1.2 Add the unplaced comparison to `thurbox-cli plugin check`: resolve at a stated reference size, report each loaded-but-unplaced plugin with its file and slot, print the `ui/layout.lua` line to add, and exit non-zero
- [x] 1.3 Exclude floats and disabled plugins from the comparison (`occupied_slots` already excludes floats and decorators; disabled files are never built) and cover both as tests, since each would otherwise be a false positive in exactly the case the check exists for
- [x] 1.4 Report the reference size in the output, so a verdict that surprises someone on a narrow terminal is explicable
- [x] 1.5 Tests: a pane whose slot no arrangement places fails; the bundled interface passes; an empty interface is still not a failure; a float and a disabled pane are not reported

## 2. Extract the delivery decision

- [x] 2.1 Extract `bundled::materialize`'s per-file decision (write / settle / update / preserve / tombstone / leave) into a pure function over (on-disk state, recorded digest, payload digest)
- [x] 2.2 Rewrite `materialize` to call it, changing no behaviour, and confirm the existing `bundled` tests pass untouched
- [x] 2.3 Add tests for the extracted function directly, covering each arm of the matrix

## 3. The spec and the lockfile

- [x] 3.1 Define the `plugins.toml` types (`src`, `file`, optional `pin`) as pure data, in `session` if the CLI and kernel both need them
- [x] 3.2 Parse `plugins.toml`, reporting a malformed spec with the file and the location of the problem, and treating an absent spec as "nothing installed" rather than a failure
- [x] 3.3 Define and read/write `plugins.lock`: per entry, the resolved source, the resolved version, and the digest of every file delivered
- [x] 3.4 Tests: a valid spec round-trips; a malformed spec fails with a location and installs nothing; a missing spec is not an error; a lock entry for an absent spec entry is detected

## 4. Provenance in the inventory

- [x] 4.1 Add `Source::Installed { src }` to `kernel::bundled` and resolve it from the spec plus the lock
- [x] 4.2 Handle the new case at every match site the compiler names — the Interface tab (`kernel::modals::interface`), `plugin list`'s output, and `sources()`
- [x] 4.3 Add `Kind::Manifest` so `plugins.toml` and `plugins.lock` are inventoried as manifests rather than falling through to `Pane`
- [x] 4.4 Tests: an installed file reports its source and is not reported as the user's own; an edited installed file reports installed and modified; the two manifests are inventoried and not reported as broken panes

## 5. Acquiring a plugin

- [x] 5.1 Reuse `extension_config::resolve_source` (and the `official_base`/`official_ref` helpers) for pane sources, with bare names resolving under `ui-plugins/` instead of `extensions/`
- [x] 5.2 Define the package shape: a directory with a `plugin.toml` (name, description, files delivered, `requires_thurbox`) plus its Lua; a single `.lua` URL as the degenerate no-manifest case
- [x] 5.3 Implement `thurbox-cli plugin install <src> [--as <file>] [--pin <version>]`: fetch, write through the extracted decision from §2, record the entry in the spec and the lock
- [x] 5.4 Refuse to write over a file the spec does not manage, naming it, and leave it exactly as it was
- [x] 5.5 Fail a bare name the official set does not contain by saying so and naming the alternatives, writing nothing
- [x] 5.6 Enforce the `lib/<package>/` convention at install time: a package may deliver shared modules only under its own namespace, so it cannot replace a shipped module
- [x] 5.7 Print the `ui/layout.lua` line to add at the moment of installing, so the instruction arrives before the check is thought of
- [x] 5.8 Tests: install by bare name records source/destination/version; install from a URL and from a local path; an occupied unmanaged destination is refused; an unknown bare name names alternatives; a package's `lib/` delivery lands namespaced and `require` resolves it

## 6. Converging, updating, removing

- [x] 6.1 Implement `plugin sync`: install absent entries, remove files it installed that the spec no longer lists, leave everything else alone, report per entry, and exit non-zero when the directory could not be converged
- [x] 6.2 Make `sync` idempotent — a second run changes nothing and reports success
- [x] 6.3 Preserve a locally modified installed file during `sync` and report it as kept rather than updated; honour a tombstone so a user's deletion is not silently reinstalled
- [x] 6.4 Implement `plugin update [<name>|--all]`: advance pins only when asked, report what moved and from what, and report "already current" as success rather than failure
- [x] 6.5 Implement `plugin remove <name>`: delete the file, its spec entry and its lock entry, report what was removed, and work without the source being reachable; fail clearly when the spec does not list it
- [x] 6.6 Tests: each of the six convergence outcomes; an unmanaged pane in the directory is left untouched and is not a problem; update reports the version it came from; remove works offline and fails on an unknown name

## 7. Trust for a managed plugin

- [x] 7.1 Extend the trust record in `kernel::registry` / `ui.json` to carry `(src, pin)` plus the digest the lock recorded, for managed files only
- [x] 7.2 Resolve trust for a managed file as trusted only when both the `src@pin` and the digest agree with the lock; report `installed · modified` when the digest differs, whether the local file changed or the source re-tagged the same pin
- [x] 7.3 Clear the grant when the pin moves, so advancing a version asks again
- [x] 7.4 Leave unmanaged files on the existing content-digest trust path, unchanged
- [x] 7.5 Tests: the full matrix from the design — granted-and-untouched, reinstalled at the same version, pin advanced, edited locally, and an upstream re-tag of the same pin (the row that is a supply-chain hole if the digest is dropped)

## 8. The first packages

- [x] 8.1 Create `ui-plugins/` in the repository and move the runnable examples into it as packages with `plugin.toml` manifests, keeping `docs/examples/composite.lua` where it is as documentation
- [x] 8.2 Verify each package installs by bare name into a scratch interface directory and passes `plugin check`, including the unplaced diagnosis when its slot is not placed

## 9. Documentation

- [x] 9.1 `docs/PLUGINS.md` — the manager, the spec format, the lockfile, what trust means for an installed pane, and why there is no lazy loading
- [x] 9.2 `ui/README.md` — replace the `cp` instructions with `plugin install`, since this is the copy the agent editing the directory reads
- [x] 9.3 `docs/CONFIG.md` — add `plugins.toml` and `plugins.lock` to the file table, stating which is hand-edited and which is machine-written
- [x] 9.4 `CLAUDE.md` — the new subcommands in the `thurbox-cli` list, and the two new interface-directory files beside `.bundled.json` / `ui.json`
- [x] 9.5 Website — the configuration page's file list, and the v2 interface page's acquisition story

## 10. Verification

- [x] 10.1 `just lint` and `just test` clean
- [x] 10.2 Install, sync, update and remove exercised end to end in a sandbox (`scripts/dev/sandbox.sh --fresh`), confirming the Interface tab shows the installed origin and the trust row reads correctly across an update
- [x] 10.3 Confirm the agent loop end to end: a session on the interface directory installs a pane, edits `ui/layout.lua`, runs `plugin check`, and gets a zero exit only once the pane is placed
