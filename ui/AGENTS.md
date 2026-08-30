# You are working in thurbox's interface

This directory **is** the running interface of thurbox, a multi-session
coding-agent orchestrator. Every pane on its screen is a Lua file here. Saving a
file reloads it; there is no build step.

`README.md` beside this file is the reference — the node kinds, the sizing rules,
what you can read and write. This file is the part that is easy to get wrong.

## "Install a plugin" means `thurbox-cli plugin install`

A *plugin* here is a thurbox interface pane, not a package from a language
registry. If someone asks you to install one:

```bash
thurbox-cli plugin available          # what installs by bare name
thurbox-cli plugin install <name>     # or a URL, or a path
thurbox-cli plugin install git+<url>  # a repository: cloned, payload and all
thurbox-cli plugin sync               # after editing plugins.toml by hand
```

A plugin that carries a program or a data file is a **repository**, and `git+<url>`
(or a `.git` suffix, or `git@host:path`) clones it into `<interface dir>/<name>/`,
keeping its `.git`. Say plainly what that does before running it: **it puts that
repository's files on the user's disk, executables included.** Nothing is executed by
installing, and a program still needs the `program` capability the user grants — but
the files are theirs now, so do not install a repository the user did not name.

`thurbox.platform` gives a pane `os` and `arch`, which is how a plugin shipping
several binaries picks one. The manifest does not do it for you.

**Never build under this directory, and never write inside an installed plugin's
working copy.** Two different reasons, both easy to trip over if you make a pane fetch
or compile something:

- This directory is watched *recursively*, so an `npm install` here fires thousands of
  events. The symptom is not "reloads too often" — a burst keeps the debounce rolling
  forward, so it **stops reloading at all** while you are busy.
- Anything you generate inside a cloned plugin's own directory makes its git tree
  dirty, and a dirty tree is what makes `plugin update` refuse to move it. You would
  be making the plugin un-updatable to save a path.

Put generated files in `$XDG_CACHE_HOME/<plugin>/` (or `~/.cache/<plugin>/`).

**There is no `npm`, `cargo`, `pip` or `go get` in this directory, and nothing to
run one on.** No `package.json`, no lockfile of that kind, no `node_modules`. The
only dependencies a pane has are the modules in `lib/` — widgets, theme, fuzzy,
textinput, chrome, modal, scroll, order, settings, panels, hover, tree, and the
session-list/path-picker/repo-picker models — which are already here and are reached with
`require("lib.theme")`. If you find yourself about to run a
package manager, you have misread the request.

`plugins.toml` records what this interface is composed of and `plugins.lock` what
each entry resolved to. You may edit the first by hand; never hand-edit the second.

## Check your work after every edit

```bash
thurbox-cli plugin check
```

It loads the interface exactly as thurbox does and **exits non-zero** on failure.
Do not report an edit as done without it. It catches two things, and the second is
the one that looks like success:

- a file that will not load, named with its reason;
- a pane that **loads and draws nothing**, because no arrangement places its slot.
  It compiles, declares its keys, appears in listings, and is absent from the
  screen. `check` prints the `layout.lua` line to add.

## Adding a pane is two edits

The plugin file, **and** its slot in `layout.lua`. A pane names a slot; the
arrangement decides where that slot goes. Miss the second and you get the
silent-but-loading failure above. `thurbox-cli plugin install` prints the line for
you.

There is a quieter version of the same failure: a slot in **`switch`** mode shows one
occupant and keeps the rest as alternates, so a pane that is not first draws nothing
until it is focused. It loads, it is placed, every check passes, and the screen does not
change. **Declare a pill** and the action band offers it:

```lua
pills = { { action = "mine.open", label = "Mine", priority = 10 } },
```

`plugin check` warns about a pane in that state and `plugin install` says it when you
install one — neither fails, because you may have meant it.

## Make the pane cost what changed, not what exists

`render` runs on the UI thread up to thirty times a second. Three habits keep a
pane from being the thing that makes the whole interface feel slow:

- **Declare `pure = true` unless the render writes.** The kernel then reuses
  your last tree until something you read changes, and skips your Lua on every
  other frame. It is only wrong if `render` writes `store`/`state` or calls
  `command` — move those into `on_key`/`on_action`/`on_click`/`on_scroll`/`on_event`. Floats
  especially: a float renders every frame *even while closed*.
- **Memoize on table identity.** The published groups (`thurbox.sessions`,
  `thurbox.theme`, `thurbox.registry`, `thurbox.bookmarks`, …) keep the *same
  table* until their data moves, so `rawequal(thurbox.sessions, cache.src)` is
  a sound one-comparison test that a derived model is still valid.
  `10_sessions`'s model and the flow's row cache are the pattern to copy.
- **Window first, build second.** Compute the visible rows (`widgets.window`)
  before building spans; hoist `store` reads, `theme.role` lookups and
  `fuzzy.compile(query)` out of per-row loops; accumulate wide strings through
  a table (`table.concat`), never `s = s .. piece` across a row.

`F12` opens the perf HUD. `renders` climbing while you touch nothing means a
pane is not settling — usually an impure render, or a per-frame `store` write
of a fresh table (writing the same *value* is free; a new table never is).

## What you cannot do from a pane

- **No `os`, `io`, `debug`, `package`, `print`, `dofile`, `load`.** They are not
  blocked, they are *missing*: `os.time()` is `attempt to index a nil value`, not a
  permission error. The VM enforces it, so `plugin check` is what catches it here —
  the static lint that also enforces it needs the thurbox checkout's own config.
- **No blocking.** Reads come from a snapshot and return instantly; writes are
  `command(...)` calls the kernel applies later. There is nothing to await.
- **No granting yourself a capability.** A pane that wants to run a program says so
  with `capabilities = { … }`, and the *user* grants it in settings
  (`Ctrl+,` → `]` → `t`). You cannot do that step for them, and you should not
  edit `ui.json` to fake it. Draw the untrusted state honestly instead.

## Do not break the way back

`layout.lua` and `lib/` are shared by every pane; a mistake there takes the whole
screen, not one pane. Prefer adding a file over editing those two. Anything shipped
with thurbox can be restored (`Ctrl+,` → `]` → `r`), so a bad edit is recoverable —
but only if you say what you changed.

A file **you** added has no shipped copy to restore, so the way back for it is
`space` on its row in that same tab: turned off, untouched on disk, and the
interface loads without it. `thurbox-cli plugin check` reports the failure with no
TTY, which is the one you can run yourself.
