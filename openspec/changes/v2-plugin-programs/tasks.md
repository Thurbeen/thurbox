## 1. The capability, declared and granted

- [x] 1.1 Add `Capability::Program` to `kernel::host`, parsed from a plugin's `capabilities` declaration like `Run`, so the inventory can report it without reading the file
- [x] 1.2 Report it in the Interface tab and in `plugin list`, distinguishably from `run` — "runs a program you interact with" rather than "runs programs"
- [x] 1.3 Confirm trust is per file and unchanged: a `run` grant must not confer `program`, and a `program` grant must not confer `run`
- [x] 1.4 Tests: the capability is enumerable from the declaration; a `run`-trusted file is still refused `program` and vice versa; the tab names what a file asks for

## 2. The pane

- [x] 2.1 Add a third window prefix (`tbp-`) beside `tb-`/`tbs-` in `agent::tmux`, and a deterministic name from a digest of the plugin path plus the pane name (digested, not sanitized — `sanitize_window_name` maps every non-`[A-Za-z0-9_-]` char to `_`, so two paths would collide)
- [x] 2.2 Add a spawn+wire path for a named program, reusing `Session::wire_up`'s parser/reader/writer rather than reimplementing it; `ProgramPane` keeps `ShellPane`'s shape
- [x] 2.3 Add `programs: HashMap<String, ProgramPane>` to `Terminals`, keyed by `(plugin path, pane name)`, with the local backend from the registry it already holds
- [x] 2.4 Make starting idempotent: asking for a pane that exists returns it, so a plugin may ask every frame
- [x] 2.5 Enforce four panes per plugin, refusing beyond it, and surface the refusal to the asking plugin
- [x] 2.6 Tests: the window name is deterministic and collision-free across plugin paths; asking twice starts one program; the fifth ask is refused and says so; releasing one frees a place

## 3. Asking for it, from Lua

- [x] 3.1 Add a `Program` command to `kernel::command` carrying the pane name, the program and its arguments, parsed the way `Shell` is
- [x] 3.2 Stamp the owning plugin from the current-plugin marker rather than accepting one from Lua, so a plugin cannot address another's pane
- [x] 3.3 Refuse the command for a plugin that does not declare the capability or is not trusted — absent rather than failing, matching how `run` is withheld
- [x] 3.4 Add a release/close command so a plugin can give a pane up deliberately
- [x] 3.5 Tests: an untrusted plugin's ask does nothing and the pane reports it; a plugin cannot start or reach a pane keyed to another plugin's path

## 4. Drawing it

- [x] 4.1 Add `SurfaceSource::Program { name }` to `kernel::node` and read `{ type = "surface", program = "..." }` in `convert`, resolving the owner from the plugin being rendered
- [x] 4.2 Extend `Terminals::render_session` with the program case, mirroring the `#shell` branch: resolve, match the pane to its rect, paint the vt100 screen
- [x] 4.3 Extend `output_stamp` to accept a program surface, so the demand-driven loop repaints when the program produces output and not otherwise
- [x] 4.4 Draw an honest placeholder for a pane with nothing behind it, and a distinct one for a program that has exited
- [x] 4.5 Start the pane at its rect's size where known, not the terminal's — the bug `open_shell` documents (born a screen wide because the shared size memo looked settled)
- [x] 4.6 Tests: a program surface paints its grid; an unstarted one draws a placeholder rather than an empty box; an exited one says so; the rect drives the size and an unchanged rect does not resize

## 5. Keys

- [x] 5.1 Generalise `Node::first_session_surface` to return the first live surface of either kind, keeping the session behaviour identical
- [x] 5.2 Route unclaimed keys from a raw-input plugin to whatever its surface named, replacing the assumption that the target is a session — `input = "session"` keeps working and now reads as "wants raw input"
- [x] 5.3 Confirm the escape route: `RESERVED` and the navigation/quit chords are never deferred to a program
- [x] 5.4 Do not report a key as handled when the pane has nothing behind it, so it is not silently swallowed
- [x] 5.5 Tests: a key reaches a focused program; an unfocused pane receives nothing; the escape chords are handled by the interface; a key aimed at an unbacked pane is not swallowed

## 6. Lifetime

- [x] 6.1 Assert a reload keeps a running program — `reload_interface` rebuilds `host` while `Terminals` is a separate field, so this holds today and must keep holding
- [x] 6.2 Release a vanished plugin's panes on reload, following `runs::retain_plugins`, killing the window: an unreachable program is worse than a closed one
- [x] 6.3 Re-adopt a pane by window name at startup, using the existing discovery-by-name path — no persisted id, so nothing can go stale
- [x] 6.4 Report an exited program and let a fresh ask start it again
- [x] 6.5 Keep quit detaching rather than killing, as it does for sessions
- [x] 6.6 Tests: a reload keeps the pane; a disabled or deleted plugin's pane is released; a restart re-finds the window by name; an exited program is reported and restartable

## 7. Not a session

- [x] 7.1 Confirm a program pane appears in no session enumeration — the session list, the count, status derivation, `thurbox-cli session list` — which holds because it is never a row
- [x] 7.2 Confirm window discovery does not adopt a `tbp-` window as an agent pane
- [x] 7.3 Tests: with a pane running, the sessions reported are exactly those reported without it, and no agent status is derived from it

## 8. Showing what it looks like

- [x] 8.1 Document the capability with a worked sketch in `docs/PLUGINS.md` — the declaration, the ask, the surface, and the untrusted state — rather than adding a pane to `ui-plugins/`. What is there is a small set of **examples**, not a catalogue, and a pane whose only purpose is to demonstrate a capability does not earn a place in it
- [x] 8.2 Correct the framing the rest of this change had introduced: `EXAMPLE_PLUGINS` rather than `OFFICIAL_PLUGINS`, and "examples you can install" rather than "the officially distributed set", in the CLI, the docs and the website
- [x] 8.3 Verify the untrusted state by hand instead — a pane that declares the capability and has not been trusted draws its own hint and starts nothing

## 9. Documentation

- [x] 9.1 `docs/PLUGINS.md` — asking for a program, the surface spelling, what the capability means and why it is not `run`, the bound, and the lifetime at each edge
- [x] 9.2 `ui/README.md` — the capability in the environment list, and the trap: a pane you did not trust has no way to ask
- [x] 9.3 `thurbox.yml` — confirm no change is needed (no new global; the ask goes through `command`) and record why, since a published field a plugin uses is meant to fail lint until declared
- [x] 9.4 `CLAUDE.md` — the capability, the third window prefix, and the key-routing generalisation
- [x] 9.5 Website — the interface page's capability story

## 10. Verification

- [x] 10.1 `just lint` and `just test` clean
- [x] 10.2 Run it for real in a sandbox: a pane holding `top`, trusted, typed at, reloaded with `F10` — confirming the program runs, takes keystrokes, and survives the reload
- [x] 10.3 Confirm the negative cases by hand: untrusted draws the hint and starts nothing; a deleted plugin leaves no running window; `thurbox-cli session list` never mentions the pane
