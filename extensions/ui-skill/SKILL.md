---
name: thurbox-ui
description: Edit the thurbox TUI — add, change or remove a pane of the running interface, which is Lua files in the thurbox interface directory (not this repository). Use whenever the user asks to change how thurbox itself looks or behaves on screen — a new pane or panel, a different layout or arrangement, a keybinding, a status line, colours or theme roles, or "add X to the sidebar". Also use for `thurbox-cli plugin` work — install, sync, check or debug an interface plugin, or a pane that loads but draws nothing.
---

# Editing the thurbox interface

> Managed by thurbox `extension install` — see **Updating this skill** at the
> bottom before editing it.

thurbox's interface has no built-in screen underneath. Every pane is a Lua file
in one directory, the kernel reads that directory at startup, and saving a file
reloads it. Changing the TUI means editing Lua there — never Rust, and never the
repository this session happens to be sitting in.

## First: find the directory

**The interface is not in the current repo.** It is a config directory of the
user's, so it is almost always outside this session's worktree:

```bash
thurbox-cli plugin dir     # the directory in force, and which rule chose it
```

Two rules pick it: `THURBOX_UI_DIR` if set, otherwise the user's own copy —
`~/.config/thurbox/ui`, or `~/.config/thurbox-dev/ui` for a dev build. Run the
command; do not assume which. If you cannot write outside this session's
worktree, say so and ask for that path to be allowed — do not edit something
else instead and report it as done.

A **thurbox checkout's `ui/` is not the live interface.** Editing `ui/` inside a
clone of the thurbox repository changes nothing on screen unless
`THURBOX_UI_DIR` points at it (`just tui-ui` in that repo does exactly that). If
the user wants their running thurbox changed, the destination is `plugin dir`'s
answer. If they want the *shipped* interface changed, it is the checkout, and
that is a code change with a pull request behind it — ask which they mean when
both are plausible.

## Then: read the directory's own docs

The directory ships the reference, and it is versioned with the binary the user
is running, so it is more current than anything you remember:

```bash
UI="$(thurbox-cli plugin dir --text | head -1)"   # or: plugin dir --json | jq -r .dir
cat "$UI/AGENTS.md"    # the operational half — what is easy to get wrong
cat "$UI/README.md"    # the reference — node kinds, the API, the traps
cat "$UI/layout.lua"   # the arrangement — which slots exist and where
ls "$UI/plugins" "$UI/lib"
```

The `--text` is load-bearing: `thurbox-cli` switches to JSON the moment its
stdout is a pipe, so a bare `plugin dir | head -1` hands you JSON, not a path.

Read `AGENTS.md` before your first edit of a session. The rest of this skill is
the short version — enough to judge a request and to not make the two mistakes
that fail silently.

## The loop: edit, then check

```bash
thurbox-cli plugin check    # loads the interface exactly as thurbox does
```

**Run it after every edit and do not report an edit as done without it.** It
exits non-zero on failure and it catches the failure that looks like success:

- a file that will not load, named with its reason;
- a pane that **loads and draws nothing**, because no arrangement places its
  slot. It compiles, declares its keys, appears in `plugin list`, and is absent
  from the screen. `check` prints the `layout.lua` line to add.

Then `thurbox-cli plugin list` shows every file, where it came from, and whether
it is drawing. In a running thurbox, `F10` reloads from disk and `Ctrl+,` → `]`
is the same inventory.

## Adding a pane is two edits

The plugin file **and** its slot in `layout.lua`. A pane names a slot; the
arrangement decides where that slot goes. Miss the second and you get the
silent failure above. `thurbox-cli plugin new <name>` writes a starter that
already loads, and `thurbox-cli plugin install` prints the `layout.lua` line for
what it installed. Nothing writes the arrangement for you — that edit is yours.

A pane sharing a **`switch`** slot is the quieter version: the slot's first
occupant is shown and yours waits until it is focused. Declare a pill and the
action band offers it:

```lua
pills = { { action = "mine.open", label = "Mine", priority = 10 } },
```

## The five rules the kernel actually enforces

1. **Four node kinds, forever** — `text`, `box`, `input`, `surface`. Lists,
   panels, tables and gauges are not node kinds; they compose in
   `lib/widgets.lua`. Wanting a fifth kind means wanting a widget.
2. **Layout resolves before render.** Size is declared statically in the
   plugin's table, never returned from `render`. That is what lets the kernel
   hand your `render` the rect it is drawing into (`ctx.width`, `ctx.height`,
   `ctx.focused`).
3. **Snapshot-read, command-write.** Reads come from an in-memory snapshot and
   return instantly; writes are `command(name, args)` calls the kernel applies
   later. Nothing you write can stall the render loop on SQLite, git or an
   unreachable host.
4. **Capabilities by absence.** An ungranted capability is *not in the
   environment* — `run` is nil, not a function that errors.
5. **Anything touching the world runs on a worker.** You ask; the answer appears
   on a later frame.

## What exists inside a pane, and what does not

Standard library: `string`, `table`, `math`, `coroutine`, `utf8`, plus the base
functions. Injected globals: `thurbox` (the snapshot — `.sessions`, `.settings`,
`.theme`, `.repos`, `.runs`, `.diffs`, `.metrics`, `.chrome`, `.platform`,
`.granted`, …), `command(name, args)`, `store`, `state`, `require` (for `lib/`
only), `files`, and `run(key, cmd, opts)` when the user has trusted the file.

**Deliberately absent**: `os`, `io`, `debug`, `package`, `print`, `dofile`,
`load`, `loadstring`, and `require` of anything outside `lib/`. They are not
blocked, they are *missing* — `os.time()` is `attempt to index a nil value`, not
a permission error. If you reach for one, the design is wrong, not the sandbox.

**There is no `npm`, `cargo`, `pip` or `go get` here, and nothing to run one
on.** A pane's only dependencies are the modules already in `lib/`. Never build
under the interface directory: it is watched recursively, and a burst of events
keeps the debounce rolling forward, so the symptom is not "reloads too often",
it is **stops reloading at all**. Generated files belong in
`$XDG_CACHE_HOME/<plugin>/`.

## Traps that cost real time

- **Asking every frame is correct.** `run(...)` and the `store.want_*` reads are
  designed to be called on every render — a fresh answer is a table lookup, not
  a process. Trying to call them "only once" gets you a pane that never updates.
- **The answer is not there yet.** `run` returns nothing useful on the frame you
  ask. Read `thurbox.runs[key]` and handle `nil` and `state ~= "done"`.
- **`run` is nil until the user trusts the file.** Declaring
  `capabilities = { "run" }` does not grant it — the user does, in settings
  (`Ctrl+,` → `]` → `t`). Check `if not run then` and draw something honest. You
  cannot do that step for them, and you must not edit `ui.json` to fake it.
- **A program pane needs `focusable = true`**, and `thurbox.granted.program`
  checked, since `command` is present whether or not you may.
- **A plugin-scoped chord does not outrank a global one.** After adding a key,
  check `F1`: if it is not listed, it did not bind.
- **`theme.*` returns roles, not colours.** Ask for `theme.accent` or
  `theme.muted` so the pane reads correctly under all thirty-six palettes. Never
  hardcode a hex value.
- **Give a pane one key that both enters and leaves it**, with
  `command("focus", { text = "review", toggle = true })`, declared
  `scope = "global"` and preferably an F-key — a focused terminal keeps the bare
  `ctrl+<letter>` chords for the program in it. A pane that only focuses itself
  is a one-way door.

## Performance: make the pane cost what changed, not what exists

`render` runs on the UI thread up to thirty times a second, so the kernel gives
every pane three levers — and a custom pane that skips them is the one thing
that can make the whole interface feel slow.

- **Declare `pure = true` unless the render writes.** A pure render reads
  `thurbox.*`, `ctx`, `store` and `state` and returns a tree; the kernel then
  reuses that tree until something it read actually changes, and skips your Lua
  entirely on every other frame. This is the single biggest lever. It is only
  wrong if `render` *writes* `store`/`state` or calls `command` — move those
  writes into `on_key`/`on_action`/`on_click` and purity comes back. Floats
  especially: a float renders every frame **even while closed**, so an impure
  closed modal costs a Lua call per frame forever.
- **Memoize on table identity, not by re-deriving.** The published groups
  (`thurbox.sessions`, `thurbox.theme`, `thurbox.registry`, `thurbox.diffs`,
  `thurbox.bookmarks`, …) keep the *same table* until their data moves, so
  `rawequal(thurbox.sessions, cache.src)` is a sound, one-comparison way to
  know your derived model is still valid. Build the model once, keep it in an
  upvalue, rebuild only when the identity changes. Never compare by
  serialising, and never cache across frames on anything *time*-based.
- **Window first, build second.** Compute which rows are visible
  (`widgets.window`) before building spans for them. Building every row of a
  long list and letting the list widget crop it does all the work for rows
  nobody sees.
- **Hoist per-row work out of the loop.** A `store.*` read crosses into Rust;
  a `theme.role(...)` lookup walks tables; `fuzzy.compile(query)` splits the
  query once so per-row matching doesn't re-split it. Read once per render,
  pass values down.
- **Strings: concatenate through a table.** `s = s .. piece` in a loop over a
  wide row is O(width²); accumulate into a table and `table.concat`, or emit
  `string.rep` runs.
- **Animation is not free-running.** `ctx.elapsed` plus the shared spinner
  helper in `lib/` follows the kernel's animation clock, which only ticks
  while something is actually animating — a pane that derives its own timer
  from elapsed on every frame re-renders forever and defeats its own `pure`.

`F12` opens the perf HUD: `renders` climbing while you touch nothing means a
pane is not settling — usually an impure render or a per-frame `store` write of
an unchanged value (writing the same value is free; writing a fresh table each
frame is not).

## "Install a plugin" means `thurbox-cli plugin install`

A plugin here is a thurbox interface pane, not a package from a language
registry:

```bash
thurbox-cli plugin available          # what installs by bare name
thurbox-cli plugin install <name>     # or a URL, or a path
thurbox-cli plugin install git+<url>  # a repository: cloned, payload and all
thurbox-cli plugin sync               # after editing plugins.toml by hand
thurbox-cli plugin remove <name>      # file, spec entry and record
```

`git+<url>` **puts that repository's files on the user's disk, executables
included.** Say so plainly before running it, and never install a repository the
user did not name. Nothing is executed by installing, and a program still needs
the `program` capability the user grants.

`plugins.toml` records what the interface is composed of; you may edit it by
hand and then run `sync`. `plugins.lock` records what each entry resolved to —
never hand-edit it. Never write inside an installed plugin's working copy
either: a dirty git tree is what makes `plugin update` refuse to move it.

## Do not break the way back

`layout.lua` and `lib/` are shared by every pane — a mistake there takes the
whole screen, not one pane. Prefer adding a file over editing those two.

Recovery, in the order it applies:

- A file thurbox **ships** can be restored: `Ctrl+,` → `]` → `r`. So no edit or
  deletion of a shipped file is unrecoverable.
- A file **you added** has no shipped copy. The way back is `space` on its row
  in that same tab: present on disk, untouched, simply not loaded — enough to
  get a working interface while it is fixed.
- An **installed** pane is put back by `thurbox-cli plugin sync`.
- With no TTY at all, the three answers are `plugin list`, `plugin dir` and
  `plugin check`.

Always tell the user which files you changed. That is what makes the first
option usable.

## Updating this skill

thurbox installs this file itself — the `ui-skill` extension is built into the
binary and on by default — and refreshes it on every start. Deleting the
"Managed by" line near the top makes the copy yours: thurbox then leaves it
alone. (`extension reinstall` and `extension install --force` overwrite
regardless — that is what they are for.) Remove every copy thurbox still owns,
for good, with `thurbox-cli extension deactivate ui-skill`.
