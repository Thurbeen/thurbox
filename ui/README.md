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
lib/            shared helpers — widgets, theme roles, fuzzy match, text input
plugins/        the panes themselves, loaded in filename order
```

## Check your work without a terminal

Do this after every edit. It loads the interface exactly as `thurbox` does and
exits non-zero on failure, so it catches the mistakes that are otherwise
invisible until something is missing from the screen.

```bash
thurbox-cli plugin check     # load it the way thurbox does
thurbox-cli plugin list      # every file, and whether it is drawing
thurbox-cli plugin dir       # which directory is live, and which rule chose it
```

In a running thurbox, `F10` reloads from disk and `Ctrl+,` → `]` shows the same
inventory `plugin list` prints.

Three rules pick the directory, in order: `THURBOX_UI_DIR`, a `./ui` beside the
working directory, then the user's own copy — `~/.config/thurbox/ui`, or
`~/.config/thurbox-dev/ui` for a dev build, which sits beside that build's own
`settings.toml` exactly as every other config path does. `plugin dir` says which
rule won, and `thurbox-cli config show` prints the resolved `ui_dir` beside the
rest of the config: worth checking before concluding an edit did nothing.

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
| `state` | your plugin's own persisted settings |
| `require` | `lib/` modules only |
| `run(key, cmd, opts)` | run a program — **only if trusted**, see below |
| `files` | bounded reads the kernel performs for you |

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
- **Declaring a key does not outrank a global one.** A plugin-scoped chord loses
  to a kernel chord. Check `F1` after adding a key: if it is not listed, it did
  not bind.
- **A slot needs a home in `layout.lua`.** A plugin whose slot appears nowhere in
  the arrangement loads fine and never draws — `plugin list` reports it as
  `no slot`, which is the fastest way to spot it.
- **`theme.*` returns roles, not colours.** Ask for `theme.accent` or
  `theme.muted` so your pane looks right under all thirty-six palettes. Never
  hardcode a hex value.
- **Instructions and memory are bounded.** An accidental infinite loop is killed,
  not hung — if a pane vanishes after an edit, check `plugin check`.

## Files here are yours, and recoverable

`.bundled.json` records what delivery did; `ui.json` records what you decided
(which files are off, which are trusted, and any rebindings). Editing a shipped
file is fine — delivery stops overwriting it once you have. Deleting one is how
you remove it, and the Interface tab (`Ctrl+,` → `]`) will `r` restore it from
the binary. So there is no way to break this directory that a restore cannot
undo.

## Full documentation

- `docs/PLUGINS.md` in the thurbox repository — writing a plugin, start to finish
- `docs/V2-KERNEL.md` — the kernel's shape and why it refuses things
- `docs/examples/composite.lua` — a worked example that runs programs
