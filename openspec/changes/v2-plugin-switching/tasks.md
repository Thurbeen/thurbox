## 1. The decision and where it is kept

- [x] 1.1 Persist a disabled set in `ui.json` beside trust and the rebindings:
      absolute path in, out and removed (`Registry::disable` / `enable` /
      `is_disabled`), with the same non-persistent-config isolation the trust
      tests use
- [x] 1.2 Tests: disabling one file does not disable another; enabling what was
      never disabled is not an error; the decision survives being written and
      read back

## 2. Not loading it

- [x] 2.1 `LuaHost::set_disabled(paths)`, mirroring `set_trusted` — the loop reads
      the decision, the host is told
- [x] 2.2 `LuaHost::build` skips a disabled file, so it is absent from `plugins`
      rather than present-and-inactive (design D2)
- [x] 2.3 The loop publishes the disabled set wherever it publishes trust, so a
      reload picks up both together
- [x] 2.4 Tests, one per "inert" scenario: a disabled plugin's key is unbound and
      claimable without a conflict; its setting is not offered; its slot is
      released; a capability it was trusted with is not granted; a plugin that
      would fail to load does not fail the interface while disabled

## 3. The state in the inventory

- [x] 3.1 `inventory::State::Disabled`, ordered with the chosen states rather than
      the faults (design D6)
- [x] 3.2 `inventory::rows` reports it — the file is on disk and deliberately not
      loaded, which is distinct from every existing reason a pane is absent
- [x] 3.3 `thurbox-cli plugin list` reports it like any other state
- [x] 3.4 Tests: a disabled file reads `disabled`, not `failed` (it is not loaded,
      and the loader must not mistake that for a load failure) and not `removed`

## 4. The Interface tab

- [x] 4.1 `space` toggles the selected file, with no confirmation, taking effect
      on the next frame
- [x] 4.2 Show the state on the row, and add the key to the tab's hint line
- [x] 4.3 Say where the files live and that adding one is putting a file there —
      including when the user has none of their own
- [x] 4.4 Tests: the toggle round-trips; toggling a file that is already off turns
      it on; the hint line offers the key

## 5. Telling the two removals apart

- [x] 5.1 Word the delete confirmation from the file's source: a shipped file is
      described as restorable, one of the user's as having no copy and being
      permanent
- [x] 5.2 Tests: the two confirmations differ in what they say about recovery, and
      the destructive one names the file
- [x] 5.3 Check `restore`'s after-the-fact message is still right, and does not
      now say something the confirmation already said better

## 6. Documentation

- [x] 6.1 `docs/PLUGINS.md`: turning a plugin off is not deleting it; which key
      does which; that a file of yours has no shipped copy
- [x] 6.2 `docs/PLUGINS.md` Traps: a disabled plugin's problems are invisible
      until it is turned back on (design D2), and its keys are free while it is off
- [x] 6.3 `CLAUDE.md`: the third thing that can be done to an interface file, and
      that disabling is a user decision in `ui.json` rather than a delivery fact
      in `.bundled.json`

## 7. Verification

- [x] 7.1 Full suite green; `cargo clippy --all-targets --all-features -D warnings`
      and rustdoc clean
- [ ] 7.2 `selene ui docs/examples`, `stylua --check ui docs/examples`, and
      `lua-language-server --check` clean — the last has never been run on this
      branch, so expect first findings
- [ ] 7.3 By hand in the sandbox (`scripts/dev/sandbox.sh --v2 --fresh`): disable a
      pane, confirm it goes and its key frees, restart, confirm it is still off,
      turn it back on; then delete one of your own and read the warning
