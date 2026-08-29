# This directory is thurbox's interface

Every pane you see in thurbox is a Lua file in here. There is no built-in
interface underneath: the kernel reads this directory, and draws whatever it
finds.

You are probably reading this because an agent was pointed at this directory to
change or add a pane. Read the rules below before editing — most of them are not
guessable from the code, and two of them are the difference between a plugin that
works and one that fails at runtime with nothing on screen to say why.

```text
layout.lua      the arrangement: which slots exist, and where
lib/            shared helpers — widgets, theme roles, fuzzy match, text input,
                border chrome, modal shells, scrolling, and the session-list
                and repo-picker models
plugins/        the panes themselves, loaded in filename order
```

## Check your work without a terminal

Do this after every edit. It loads the interface exactly as `thurbox` does and
exits non-zero on failure, so it catches the mistakes that are otherwise
invisible until something is missing from the screen.

```bash
thurbox-cli plugin check     # load it the way thurbox does
thurbox-cli plugin list      # every file, where it came from, whether it is drawing
thurbox-cli plugin dir       # which directory is live, and which rule chose it
```

`check` fails on two things, and the second is the one worth knowing about. A file
that will not load is obvious. A pane that **loads and draws nothing** — because no
arrangement places its slot — is not: it compiles, declares its keys, appears in
`plugin list`, and is absent from the screen. `check` reports it with the
`layout.lua` line to add, and exits non-zero, so it is caught rather than puzzled
over.

In a running thurbox, `F10` reloads from disk and `Ctrl+,` → `]` shows the same
inventory `plugin list` prints.

Two rules pick the directory: `THURBOX_UI_DIR` if set, otherwise your own copy —
`~/.config/thurbox/ui`, or `~/.config/thurbox-dev/ui` for a dev build, which sits
beside that build's own `settings.toml` exactly as every other config path does.
`plugin dir` says which rule won, and `thurbox-cli config show` prints the resolved
`ui_dir` beside the rest of the config: worth checking before concluding an edit did
nothing.

Standing in a checkout does **not** change which interface loads. There used to be a
third rule — a `./ui` beside the working directory won automatically — and it made
the interface the one config that ignored the dev/release split: `cargo run` in the
repository read `~/.config/thurbox-dev` for agents, settings and the database, and
the checkout for its panes. Editing a checkout's interface is
`THURBOX_UI_DIR=ui` now (`just tui-ui` in the repository), which is the same request
said out loud.

## The five rules

1. **Four node kinds, forever**: `text`, `box`, `input`, `surface`. Lists,
   panels, tables and gauges are *not* node kinds — they compose from these in
   `lib/widgets.lua`. If you find yourself wanting a fifth kind, you want a
   widget.
2. **Layout resolves before render.** Size is declared *statically* in the
   plugin's table, never returned from `render`. That is what lets the kernel
   tell your `render` the rect it is drawing into.
3. **Snapshot-read, command-write.** Reads come from an in-memory snapshot and
   return instantly. Writes are commands, accepted now and applied later. Lua
   never blocks, so nothing you write here can stall the render loop on SQLite,
   git, or an unreachable host.
4. **Capabilities by absence.** An ungranted capability is *not in the
   environment* — not a function that returns an error. See below.
5. **Anything touching the world runs on a worker.** You ask; an answer appears
   on a later frame.

## A whole plugin

```lua
local theme = require("lib.theme")
local widgets = require("lib.widgets")

return {
  name = "example",     -- what the focus ring and help call it
  slot = "center",      -- which slot in layout.lua it occupies
  order = 90,           -- draw/focus order within that slot
  focusable = true,

  keys = {
    { key = "f8", action = "example.open", desc = "show the example",
      scope = "global" },
  },

  settings = {
    { id = "loud", desc = "say it twice", default = false },
  },

  -- Reachable from the Ctrl+P palette without spending a chord.
  commands = {
    { action = "example.open", desc = "show the example" },
  },

  -- Told once per change, off the render path: `thurbox-cli plugin events`.
  events = { "session.status" },
  on_event = function(name, payload)
    if payload.to == "blocked" then
      state.needs_you = payload.session
    end
  end,

  render = function(ctx)
    -- ctx carries the RESOLVED rect: ctx.width, ctx.height, ctx.focused.
    local rows = {}
    for _, session in ipairs(thurbox.sessions) do
      rows[#rows + 1] = { spans = { { text = "  " .. session.name,
        style = { fg = theme.text } } } }
    end
    return {
      type = "box",
      frame = widgets.panel("Example", ctx.focused),
      children = { widgets.list({ rows = rows, height = ctx.height - 2 }) },
    }
  end,

  on_action = function(action)
    if action == "example.open" then
      command("focus", { text = "example" })
      return true    -- handled; returning false lets it fall through
    end
    return false
  end,
}
```

Save it as `plugins/90_example.lua` and it is a pane. `thurbox-cli plugin new
<name>` writes a starter that already loads.

## What you have to work with

The whole environment. Anything not in this list does not exist here.

**Lua standard library**: `string`, `table`, `math`, `coroutine`, `utf8`, plus
the base functions (`pairs`, `ipairs`, `type`, `tostring`, `tonumber`, `select`,
`next`, `assert`, `error`, `pcall`, `xpcall`, `setmetatable`, `getmetatable`,
`raw*`).

**Injected globals**:

| Global | What it is |
|---|---|
| `thurbox` | the read side — the snapshot. `thurbox.sessions`, `.settings`, `.theme`, `.repos`, `.runs`, `.diffs`, `.metrics`, `.chrome`, `.ui_dir`, … |
| `command(name, args)` | the write side. Accepted now, applied by the kernel later. |
| `store` | scratch state that survives a frame, and how you *ask* for things (`store.want_branches`, `store.want_content`, …) |
| `state` | your plugin's own scratch state — survives a reload, **not** a restart |
| `require` | `lib/` modules only |
| `run(key, cmd, opts)` | run a program and read its output — **only if trusted**, see below |
| `files` | bounded reads the kernel performs for you |
| `thurbox.platform` | `os` and `arch`, so a plugin shipping several builds can pick one |

**Events**: declare `events = { "session.status", … }` and an `on_event(name,
payload)` and the kernel calls you once per change, with the same environment a
render has. `command("emit", { text = "x", … })` reaches every plugin subscribed
to `user.x`. `commands = { { action, desc } }` puts an action in the `Ctrl+P`
palette with no chord. The list of events is `thurbox-cli plugin events`.

`thurbox.granted` tells you which capabilities *this* file has been granted
(`granted.run`, `granted.program`). It exists because not every capability can be
withheld by absence: `run` is a global, so `if not run then` is the check, but an
interactive program pane is asked for through `command`, which every plugin has.

**Deliberately absent**: `os`, `io`, `debug`, `package`, `print`, `dofile`,
`load`, `loadstring`, `require` of anything outside `lib/`. They are not blocked,
they are *missing* — `os.time()` is an `attempt to index a nil value`, not a
permission error. `selene` catches this at lint time via `thurbox.yml`, which is
the sandbox written down; run `selene ui` if it is installed.

## Traps

These are the ones that cost real time.

- **Asking every frame is correct.** `run(...)` and the `store.want_*` reads are
  designed to be called on every render: a fresh answer is a table lookup, not a
  process. Do not try to call them "only once" — you will get a pane that never
  updates.
- **The answer is not there yet.** `run` returns nothing useful on the frame you
  ask. Read `thurbox.runs[key]` and handle `nil` and `state ~= "done"` — a pane
  that assumes the answer is present renders an error on its first frame.
- **`run` is nil until the user trusts the file.** Declaring
  `capabilities = { "run" }` does not grant it. Check `if not run then` and draw
  something honest; do not call it and hope.
- **A program pane needs `focusable = true`.** `capabilities = { "program" }` plus
  `input = "session"` gets you a terminal you can never type at, because raw input
  only reaches the plugin that *has* focus. It draws perfectly and ignores the
  keyboard, which is a confusing thing to debug — and check
  `thurbox.granted.program`, since `command` is present whether or not you may.
- **Declaring a key does not outrank a global one.** A plugin-scoped chord loses
  to a kernel chord. Check `F1` after adding a key: if it is not listed, it did
  not bind.
- **A slot needs a home in `layout.lua`.** A plugin whose slot appears nowhere in
  the arrangement loads fine and never draws. **Adding a pane is two edits** — the
  plugin and the slot — and `plugin check` fails on the missing second one and
  prints the line to add.
- **A pane sharing a `switch` slot draws nothing until it is focused.** The quieter
  sibling of the above: the slot's first occupant is shown and yours waits. Nothing
  fails, so declare a `pills = { … }` entry and the action band will offer it —
  `plugin check` warns when you have not.
- **Give that pane one key that both enters and leaves it**, with `toggle`:

  ```lua
  command("focus", { text = "review", toggle = true })
  ```

  Pressed once it focuses your pane; pressed again it returns to whatever was
  focused before. Declare it `scope = "global"` so it works from inside a focused
  terminal, and prefer an F-key — a focused terminal keeps the bare
  `ctrl+<letter>` chords for the program in it.

  The alternative is worse in a way that is not obvious: a pane that focuses
  *itself* and leaves the user to walk out with `ctrl+h`/`ctrl+l` is a one-way
  door, and a pane that focuses a named sibling to get back has hard-coded the
  user's arrangement. `toggle` uses the memory `Esc` already uses, so it needs no
  name and `Esc` keeps working too.
- **`theme.*` returns roles, not colours.** Ask for `theme.accent` or
  `theme.muted` so your pane looks right under all thirty-six palettes. Never
  hardcode a hex value.
- **Instructions and memory are bounded.** An accidental infinite loop is killed,
  not hung — if a pane vanishes after an edit, check `plugin check`.

## Panes somebody else wrote

You do not have to write one from scratch. `plugins.toml` here lists what this
interface is composed of, and the manager keeps the directory in step with it:

```bash
thurbox-cli plugin available          # what installs by bare name
thurbox-cli plugin install top        # fetch it, and record it in plugins.toml
thurbox-cli plugin sync               # make the directory match the spec
thurbox-cli plugin remove top         # file, spec entry and record
```

`install` prints the `layout.lua` line the new pane needs, since that is the edit
it cannot make for you — **nothing here writes your arrangement.** After editing
`plugins.toml` by hand, `sync` is the one command to run: it installs what is
missing, takes back what you removed from the spec, and leaves everything else
alone. Running it twice changes nothing.

Your edits survive all of it. A managed file you changed is reported as `kept` and
never overwritten, and a managed file you deleted stays deleted — that is how you
remove one.

`plugins.lock` beside the spec records what each entry resolved to. You edit the
spec; nothing edits the lock. Commit both and this interface reproduces elsewhere.

## Files here are yours, and recoverable

`.bundled.json` records what delivery did; `ui.json` records what you decided
(which files are off, which are trusted, and any rebindings). Editing a shipped
file is fine — delivery stops overwriting it once you have. Deleting one is how
you remove it, and the Interface tab (`Ctrl+,` → `]`) will `r` restore it from
the binary. So no edit or deletion of a file thurbox ships is unrecoverable.

A file **you** added is the case `r` cannot help with — thurbox ships no version
of it to put back, and it says so instead. The way back there is `space` on its
row: the file stays exactly where it is and is simply not loaded, which is enough
to get a working interface while you fix it. An installed pane
(the row reads `from <src>`) is put back by the manager that placed it,
`thurbox-cli plugin sync`. The tab sorts failures to the top and shows the load
error of the selected row, so the broken file is the first thing on the list.

## Full documentation

- `docs/PLUGINS.md` in the thurbox repository — writing a plugin, start to finish
- `docs/V2-KERNEL.md` — the kernel's shape and why it refuses things
- `examples/lua/composite.lua` — a worked example that runs programs
