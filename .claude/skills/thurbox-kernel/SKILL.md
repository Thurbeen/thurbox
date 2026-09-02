---
name: thurbox-kernel
description: The thurbox v2 plugin kernel: the five rules, module dependency rules enforced by tests/architecture_rules.rs, module responsibilities, the event loop, and writing an interface plugin - the bundled panes, delivery vs decision vs composition, installable panes and plugins.toml, capabilities (run, program, events, commands) and trust. Use when working on src/kernel, src/coordinator, module boundaries, or the plugin API and its capabilities.
---

# Thurbox plugin kernel and plugin authoring

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Architecture (plugin kernel)

The interface is **Lua running on a Rust kernel**. `thurbox` boots the kernel,
which reads `ui/` and renders whatever plugins it finds; there is no built-in
pane. v1's `src/app` (TEA model/update/view) and `src/ui` (35 render modules) were
deleted when the kernel took the binary name — v1 lives on the `v1.x` branch.

### The five rules

1. **Four node kinds, forever** — `text`, `box`, `input`, `surface`. Everything
   else composes in `ui/lib/widgets.lua`. `tests/kernel_mvp.rs` asserts the count.
2. **Layout resolves before render** — rects are computed first, then each plugin
   is called with its own. Plugins declare size *statically*, in their declaration
   table, which is what breaks the circularity.
3. **Snapshot-read, command-write** — reads come from an in-memory snapshot and
   return instantly; writes are commands accepted now and surfaced later. Lua never
   blocks, so no plugin can stall the loop on SQLite, git or an unreachable host.
4. **Capabilities by absence** — an ungranted capability is *not in the
   environment*. `io`, `os`, `debug`, `package` and the loaders are withheld, and
   `thurbox.yml` makes selene enforce that statically. One capability cannot be
   expressed that way: a program pane is asked for through `command`, which every
   plugin has, so `thurbox.granted` reports the grant instead — a boolean about a
   decision already made, which withholds nothing.
5. **Anything touching the world runs on a worker** — terminal attach, commands,
   diffs, metrics, git stats, repository reads, update checks, and programs a
   plugin asked for.

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no crate-internal references
agent    ← session (+ paths/shell utils; NEVER git)
kernel   ← session + storage + sync + paths + session_ops + git
           (+ agent/usage by fully-qualified path only)
main     ← the coordinator: the loop, the workers, the chrome
```

Enforcement is an **allowlist**: every module under `src/` needs a `ModuleRules`
entry naming what it may reference in *any* form (`use`, `pub use`, brace groups,
fully-qualified `crate::…`), so a new module fails the test until its place is
declared. `main` is `EXEMPT`, as `app` was before v1 was retired. `kernel` reaches
`agent`/`usage` by fully-qualified path only — never `use` — so every crossing into
the side-effect layer is visible at its call site, the rule `session_ops` and `cli`
already follow.

### Module Responsibilities

- **`kernel/`** — the interface. `node` (four primitives), `layout` (rects before
  render), `convert` (Lua table ↔ node), `paint` (node → ratatui), `host` (the VM,
  reload, isolation, capability grants), `registry` (keys + settings plugins
  declare, plus the chord-less commands the palette lists), `events` (the
  closed set a plugin may subscribe to, and the deriver that diffs a snapshot
  into them), `modals/` (help, settings, theme picker, the command palette —
  chrome about thurbox itself,
  which plugins contribute *data* to rather than replace), `bands` (the top/bottom
  bars), `snapshot` (the read side), `command` (the write side), `terminal/` (live
  PTY surfaces: the attach machinery, plugin program panes, link detection +
  OSC 8 painting), `selection` (mouse text selection over a pane),
  `consent` (the one-time v1→v2 gate), plus the worker-backed
  stores: `diff`, `metrics`, `repos`, `runs`, `updates`, `files`, `notify`,
  `theme`, `perf`, `bundled`, `inventory`, `packages` (the spec, the lock, and the
  install/converge/withdraw operations — sharing `bundled`'s decision matrix).
- **`agent/`** — side-effect layer, unchanged by the retirement. `AgentProvider`
  - `GenericProvider` build the CLI invocation from a declarative `AgentDef`;
  `Session` wraps a `SessionBackend`; `TmuxBackend` runs tmux over a
  `TmuxTransport` (`Local` / `Ssh` / `Wsl`). Output is read into
  `Arc<Mutex<vt100::Parser>>`, input written over an mpsc channel.
- **`session/`** — plain data: `SessionId`, `SessionStatus`, `SessionInfo`,
  `SessionConfig`, `AgentDef`/`AgentRegistry`, `HostDef`/`HostRegistry`,
  `PluginSpec`/`PluginLock`/`PackageManifest` (`plugin_spec`, so the kernel and the
  CLI share one definition), plus the logic the kernel needs and cannot import
  `agent` for (`hyperlink`, `review`, `theme_config`).
- **`git/`** — every `git` invocation, split by concern: `command` (the one
  place a `git` process is built, and where the inherited `GIT_*` scrub lives),
  `plugin` (clone/checkout of a plugin working copy), `remote` (running a command
  over ssh/`wsl.exe`, the PowerShell encoding, and decoding what came back —
  ADR-13's psmux divergences), `discovery` (listings, path classification,
  child-repo scans, each a POSIX/PowerShell script pair over one line protocol),
  `diff` (diffs, branches, commits, worktree stats) and `worktree` (create, list,
  sync, remove — and the stale-lock/stash/transient-error retries). `git::*` is still
  one flat surface; no caller names a submodule.
- **`coordinator/`** — `main`'s own body. `App` and its state stay in
  `main.rs`; its behaviour is here, grouped by what it is for: the loop and its
  workers (`mod`), then `commands`, `publish`, `draw`, `input`, `mouse`, `focus`,
  `events` (the one dispatch point, and the kernel events derived from the
  loop's own signals) and `interface` — plus `boot` (the startup sequence `main` delegates to),
  `chrome` (the terminal/error-panel/perf-HUD helpers) and `editor` (the
  `Ctrl+O` command resolution). Still one model and one loop — splitting `App`
  itself is what ADR-22 rejected, and the invariants that matter hold *across*
  these groups.
- **`ui/`** (Lua, not Rust) — `layout.lua` is the arrangement; `lib/` holds
  widgets, theme roles, fuzzy match, text input, trees — plus the extracted
  pane halves: `chrome` (borders/cells), `modal` (frame + footer), `scroll`,
  `order` and `session_model` (the session list's model, with its memo),
  `pathpicker` and `repo_picker` (the creation flow's); `plugins/` holds the
  panes.
- **`cli/`** — `thurbox-cli` subcommands, including `plugin dir|new|check|list|events|install|sync|update|remove`
  for writing an interface with no TTY.

### Event Loop (`src/main.rs` + `src/coordinator/`)

```text
tokio::main → load config + settings → heal extensions → arm the heartbeat
  → the v1→v2 consent gate (kernel::consent, before the terminal is taken)
  → resolve ui/ → build the Lua host → open SQLite → init terminal → loop {
    resolve layout → call each plugin with its rect → paint
    → poll workers (terminals, commands, diffs, metrics, repos, runs, updates)
    → dispatch events to the plugins subscribed to them (kernel::events)
    → drain Lua's command queue → dispatch keys through the registry
} → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log` (stdout is the TUI's)
- A panic hook restores the terminal, pops the kitty flags and disables mouse
  reporting before printing — otherwise the shell inherits a raw-mode terminal
  streaming mouse reports. A **signal** gets the same treatment
  (`coordinator::boot::install_signal_restore`): `SIGHUP`/`SIGTERM`/`SIGINT`
  run `restore_terminal` and exit `128 + n`, since the default action runs no
  hook and a terminal emulator closing, a session manager, or a machine waking
  from a long sleep used to leave the next shell printing `\x1b[<64;…M` on
  every scroll. `restore_terminal` shows the cursor itself for the same
  reason — a signal exits from the runtime's thread and drops no `Terminal`.
  The one case no handler reaches is a **dropped ssh connection** to a remote
  thurbox: the `?1003l` has no pty to travel down, so the *local* emulator
  keeps reporting — `reset` there, or run the ssh session inside a local tmux.
  `tests/tui_e2e.rs` pins both exits' escapes.


## Writing an interface plugin

The bundled set is deliberately small: `10_sessions`, `20_agent`, `65_search`, plus
three floats that occupy no slot — the creation flow (`70_new_session`), the
confirmation (`60_confirm`) and the restore list (`80_restore`, v1's `Ctrl+U`).

```bash
thurbox-cli plugin dir            # which directory is live, and which rule chose it
thurbox-cli plugin new notes      # a starter that already loads
thurbox-cli plugin check          # load it the way thurbox does; non-zero on failure
thurbox-cli plugin list           # the inventory the Interface tab shows
thurbox-cli plugin install top    # a distributed pane, recorded in plugins.toml
thurbox-cli plugin sync           # make the directory match the spec
```

Two of v1's surfaces are maintained as **out-of-tree panes**, each its own
repository, installed by clone:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) (the diff
reviewer, asks for `run` only for the targets the kernel does not compute) and
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) (v1's info
panel, no capabilities — everything from the snapshot). Neither is bundled or
vendored; both are downstream consumers of the published `thurbox.*` shape.

Two **example** panes live in `examples/panes/` (`tasks`, `top`) and install by bare
name; `examples/lua/{plugin,composite,events,layout}.lua` are copied by hand. They are
examples to read and copy from, not a catalogue thurbox maintains for anyone —
`EXAMPLE_PLUGINS` is the list a bare name and a typo suggestion resolve against.
`check` fails on a pane that **loaded but which no arrangement places** — the only
failure with no symptom — and prints the `layout.lua` line to add.

The directory ships its own guidance for whoever edits it: `README.md` is the
reference, and **`AGENTS.md`** is the operational half a coding CLI loads as context
without being asked — which is what stops "install this plugin" being read as a
package-manager request rather than `thurbox-cli plugin install`. `CLAUDE.md` and
`GEMINI.md` beside it are one-line pointers, not copies, so there is one file to
keep true; the `flow` extension surfaces its own spec the same way, as symlinks it
can make because it copies its own files.

Two rules pick the directory: `THURBOX_UI_DIR` if set, otherwise the user's copy
(`~/.config/thurbox/ui/`, materialised from the embedded interface on first run,
preserving edits). A third rule — a `./ui` beside the working directory, winning
automatically — was removed: it made the interface the one config that ignored the
dev/release split, so `cargo run` in the repository read `~/.config/thurbox-dev` for
agents, settings, themes and the database and the *checkout* for its panes, silently.
Editing a checkout's interface is now an explicit `THURBOX_UI_DIR` (`just tui-ui`),
and startup reports the directory whenever there is a question about it — an override
is in force, the fallback was used, or this is a dev build. The user-copy path is
derived
from `paths::config_file()` like every other config path — `agents.toml`,
`hosts.toml`, `hooks.toml`, `settings.toml`, `themes.toml`, `extensions/`, `ui.json` — so a **dev
build reads `~/.config/thurbox-dev/ui`** and cannot touch the release copy.
`THURBOX_CONFIG_DIR` overrides that anchor and thurbox injects it into every
session it spawns, which is why a `thurbox-cli` run *inside* a session resolves
against that session's config dir rather than the dev default. `config show` prints
the resolved `ui_dir`/`ui_json` beside the rest. The resolved directory is
made **absolute** — trust, the disabled set and rebindings are keyed by
`ui_dir.join(file)` and compared verbatim, so a relative `ui` would be shared by
every checkout on the machine. It is *not* canonicalised: that would return a
`\\?\D:\…` extended-length path on Windows and resolve `/var` to `/private/var` on
macOS, and this path is shown to people.

**Delivery vs. decision vs. composition.** `.bundled.json` records what delivery did
(bundled / edited / yours / removed / installed); `ui.json` records what the *user*
decided (disabled, trust, rebindings); `plugins.toml` records what the interface is
**composed of** and `plugins.lock` what each entry resolved to.
Deleting a bundled file is how you remove it — delivery records
the removal and never writes it back, which is what makes a differently-named
replacement possible on equal terms. Turning one **off** is a third thing: present on
disk, intact, not loaded — implemented by `build` not reading the file, so a disabled
plugin declares no keys, occupies no slot and is granted no capability. A broken one
can be switched off to get a working interface back.

**A plugin that carries more than Lua is a repository** (`ExtensionSource::Git`,
`git::clone_plugin`). The fetch path returns a `String` and decodes remote output
lossily, so a binary through it is *corrupted rather than refused* — which is why
payload arrives only by clone and that path stays Lua-only. Recognition is
**explicit**: `git+<url>`, a `.git` suffix, or `git@host:path`; a bare `https://` URL
keeps meaning "fetch the manifest's files from this base". The working copy lands at
`<ui_dir>/<name>/` and **keeps its `.git`**, so `update` is a fetch and git owns "your
edits are yours" (a dirty tree is never moved and reports `kept`). The lock records
the **commit**, not the ref, which is what makes a spec reproducible and why there is
no checksum field. Two consequences elsewhere: `build` loads panes the spec names
outside `plugins/` (`is_nested_pane`; the load order still comes from the basename's
numeric prefix) and `sources()` inventories those panes but deliberately does **not**
walk the rest of a working copy. `.git` is watcher noise (`watch::is_noise`) — a fix
in its own right, since it already affected anyone versioning their own panes, and the
symptom is not "reloads too often" but "stops reloading while git is busy".
`thurbox.platform` is published so a plugin shipping several binaries picks one
itself; platform selection is deliberately not a manifest field.

**Panes are installable** (`kernel::packages`, `session::plugin_spec`). `plugins.toml`
in the interface directory lists a `src` (a bare name resolving to `examples/panes/<name>`
at the binary's release tag, a URL, or a path — `extension_config::resolve_source_in`,
one resolver for both kinds of thing), a destination `file`, and an optional `pin`;
`plugins.lock` records what each resolved to plus the digest of every file delivered.
`plugin sync` converges the directory to the spec, which reduces an agent's job to
*edit one file, run one command, read the exit status*. Delivery semantics are
**shared with the bundled path, not reimplemented**: both call `bundled::decide`, so an
edit is preserved and a deletion is remembered for a package exactly as for a shipped
file. Two consequences worth knowing: the manager never writes `layout.lua` (placement
stays hand-authored, and `check` fails loudly on an unplaced slot instead), and a
package may deliver shared modules only under `lib/<its own name>/` — enforced at
install time, since `require` splits on every dot and would otherwise let a package
replace `lib/theme.lua`.

**Trust for an installed pane is keyed to `(src, pin)` *and* the lock's digest**
(`registry::Granted::Managed`, `inventory::Trust::resolve_installed`). Contents alone
would report every ordinary release as tampering; `(src, pin)` alone would let an
upstream re-tag of the same version inherit a grant made to what that version used to
be. Both are checked, and `ui.json`'s bare-string grant form is still read so an
upgrade forgets nothing.

**A plugin can run a program you interact with** (`Capability::Program`,
`kernel::terminal`'s `programs` map). `run` captures output once with no stdin and
no tty, so it cannot give you `htop`, `lazygit` or a REPL; a *program pane* holds a
real terminal — keystrokes go to it, it is resized to its rect, it survives an
`F10` reload. The pane belongs to the **plugin**, not a session: the plugin writes
`command("program", { text = "watch", repo = "htop" })` and draws
`{ type = "surface", program = "watch" }`, and the kernel stamps the owner from the
plugin being rendered, so naming another plugin's pane is impossible by
construction. Reuses the companion-shell machinery whole (`ProgramPane` mirrors
`ShellPane` over the same `Session::wire_up`); the differences are a third window
prefix (`tbp-`, invisible to `discover`'s `tb-` filter — hence `find_window`), and
that **nothing is persisted**: the window name is deterministic, so re-adoption
after a restart is a lookup and there is no stored id to go stale. Gated by its own
capability, **not** `run`'s — `run` is bounded (256 KB, 600 s, 4 at a time) and an
interactive program is none of those, so an existing grant must not silently widen.
Four panes per plugin. `thurbox.granted.<name>` is how a pane knows, since
`command` is present whether or not it may.

**A plugin can run a program** (`kernel::runs`) — `git status`, `docker compose ps` —
in the session's working directory, and on that session's own host for a remote
session. `run(key, program, opts)`; the answer arrives next frame as
`thurbox.runs[key]`, so Lua still never blocks, and asking **every frame is the
intended pattern** because a fresh answer is a map lookup rather than a process
(`request` refuses a duplicate while the answer is fresh *or* while a run for that
key is in flight). Bounds are the kernel's: output capped with truncation flagged, a
timeout, four at a time with the rest queued.

This is the first capability that reaches outside thurbox, so it is granted **per
plugin**: declare `capabilities = { "run" }` and get nothing until the user trusts
the file (settings → Interface → `t`). Trust is keyed by absolute path with the
digest recorded, so a changed trusted file reads `trusted · modified`. It is
deliberately **not a sandbox** — a program thurbox spawns has the user's authority —
and the position is that thurbox can only refuse to run things unasked.
`examples/lua/composite.lua` is the worked example.

The implementation lives per-call: `LuaHost::enter` stamps the current plugin and, in
the same breath, binds `run` to the implementation or to nil, and `enter_nothing` is
the other half for Lua that belongs to no plugin (`layout.lua` declares no
capabilities). The implementation itself is held in the VM's **registry**, never its
globals, because a plugin chunk's `_ENV` *is* the globals table — it sat there as
`__run_impl` once, which handed every untrusted plugin the capability under a second
name.

**A plugin can react to events** (`kernel::events`, `coordinator::events`).
Declare `events = { "session.status", … }` and an `on_event(name, payload)`, and
the loop calls it once per change, off the render path, under the render's
budget. The kernel's events are **derived by diffing the snapshot** — a row
appearing, leaving, changing status/name/branch/repos/parent — plus the focus
ring, the command bus (`command.done`/`failed`, and `session.post_*` for the
four operations *this* interface performed, named as `hooks.toml` names them) and
`interface.reloaded`; never raised from `session_ops`, so a session another
process made fires the same event. The set is closed (`KERNEL_EVENTS`): a
subscription to an unknown name refuses to load, and the same table renders in
`F1` and `thurbox-cli plugin events`. `command("emit", { text = name, … })`
delivers `user.<name>` to other plugins with `source` stamped by the kernel;
cascades stop at `MAX_DEPTH`. Dispatch never marks the frame dirty itself
(`frame-cost`); a reload drops the queue and delivers `interface.reloaded` first.
`examples/lua/events.lua` is the worked example.

**A plugin can declare chord-less commands** — `commands = { { action, desc } }`
— which the **command palette** (`Ctrl+P`, `kernel::modals::palette`) lists
beside every declared key and the kernel's own actions, filtered by subsequence
as you type; `Enter` runs the row through the same `on_action` a key takes,
focused or not, after the modal has closed. A command a user rebinds from `F1`
becomes a binding (`Registry::apply_overrides` synthesises it). `Ctrl+P` was
taken deliberately from the chords held for v1's panes (`tests/keymap.rs`),
and the creation flow's folder import moved to `Alt+P` for it.

- `docs/V2-KERNEL.md` — the kernel's shape, its five rules, and the traps
- `docs/PLUGINS.md` — writing a plugin; **Start here** needs no TTY, and **Traps**
  lists the mistakes that are invisible until runtime

