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
                focus styling, modal shells, scrolling, and the session-list
                and repo-picker models
lib/thurbox.d.lua   the API as types: node props, ctx, the event payloads, every
                published `thurbox.*` row, every command verb's options, and the
                theme roles. Declarations only — nothing loads it at runtime
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
   widget. New *appearances* arrive as props on these four: a `text` node takes
   a `style` painted across its whole rect before the spans go on top, which is
   what a selection bar or a hover band is — nothing pads a row with a spacer
   span, and a span that names its own colour keeps it. `widgets.list` takes
   `selected_style` / `hover_style` and puts them there for you.
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
local ui = require("lib.ui")

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
    local items = thurbox.sessions
    return ui.panel({
      title = "Example",
      focused = ctx.focused,
      body = ui.list({
        items = items,
        cursor = ui.cursor("example", items),
        width = ctx.width - 2,
        height = ctx.height - 2,
        on_overflow = "rows",
        row = function(session)
          return ui.row({ width = ctx.width - 2 })
            :add(" " .. session.name, { fg = theme.text })
            :trailing(session.branch, { fg = theme.muted })
            :spans_list()
        end,
        empty = ui.empty({ title = "Nothing here yet", width = ctx.width - 2 }),
      }),
    })
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

## The component layer (`lib/ui.lua`)

`lib/widgets.lua` is the primitive kit — measurement, windowing, a row of text.
`lib/ui.lua` is the layer above it: the shapes a pane turns out to be, with the
conventions that make six panes look like one program. Reach for it first; drop
to `widgets` for the piece it does not cover.

| | what it is |
|---|---|
| `ui.panel{title, focused, body, overlay_left, overlay_right, right_column, border, title_align}` | a framed pane in the one focus convention. Focus is a brighter border and a title badge, never a marker glyph |
| `ui.list{items, cursor, width, height, row, header, empty, on_overflow, pad, len, fill}` | a scrolling list. Variable row heights (`header` glues a group heading to its first row), the selection bar, hover, and the window arithmetic |
| `ui.cursor(key, items, opts)` | `{index, offset, follow}` over a list, with `move`/`select`/`select_by_id`/`follow`. `opts.steer` is the `store` key another pane moves this list with; `opts.request` a one-shot "go to this row" |
| `ui.row{width, tone}` | a span builder that knows the row's columns: `:add`, `:gap`, `:button`, `:match` (search hits), `:trailing` (a note budgeted against what is left), `:spans_list` |
| `ui.empty{title, width, hint, hint_action}` | the one empty state — a blank line, the sentence centred, and the chord that fixes it, shown only while something is bound to it |
| `ui.modal{title, cols, children, crumbs, border}` | a float sized from what is in it |
| `ui.footer{actions, primary, cancel}` | key hints resolved **from the registry**, plus the confirm/dismiss pills |
| `ui.status(name, elapsed)` / `ui.dots(items, elapsed, status_of)` | a status glyph, spinner included; and the strip of them a panel puts on its border |
| `ui.rule(label, width)` | `── label ────`, the group heading |
| `ui.chord(action)` / `ui.describe(action)` / `ui.follow(key, id)` / `ui.reset(key)` | the registry lookups, and a cursor addressed without holding one |

Two things the layer decides so a pane does not: a **selected row is a
full-width bar** (the row's own `style`, so a span that names a colour keeps it),
and **hints come from the key registry**, so a rebind moves the hint and a
removed action takes its hint with it.

`10_sessions.lua` and `80_restore.lua` are the two worked examples — a
full-height pane and a float.

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
| `text` | display width in COLUMNS: `text.width`, `text.truncate`, `text.pad` |
| `store` | scratch state that survives a frame, and how you *ask* for things (`store.want_branches`, `store.want_worktrees`, `store.want_content`, …) |
| `state` | your plugin's own scratch state — survives a reload, **not** a restart |
| `require` | `lib/` modules only |
| `run(key, cmd, opts)` | run a program and read its output — **only if trusted**, see below |
| `files` | bounded reads the kernel performs for you |
| `thurbox.platform` | `os` and `arch`, so a plugin shipping several builds can pick one |

**Borders**: a node's `frame` is the whole border vocabulary — `title` (styled
runs, not a bare string), `title_align`, `border_type` (`rounded` or `square`),
`border_style`, `padding`, and `overlay`. The overlay paints runs onto the
frame's *own* border cells after the block draws them —
`top_left`/`top_right`/`bottom_left`/`bottom_right` along the horizontal borders
and `right_column` one run per inner row down the right one — so a status strip,
a scroll count or a scrollbar costs no content cell. Slots clip between the
corners, which are never painted over.

**Clickable spans**: a run inside a line takes `id`/`role` of its own and becomes
a target over the columns it is laid out at, so a chip needs no node with a
hand-computed `len`. Adjacent runs carrying the same identity coalesce into one
hitbox — which is how ` ◀ F9 `, two runs because a run has one style, stays one
button. It applies to overlay runs too: that is what makes the terminal pane's
scrollbar one target the length of its column.

**Dragging**: a node with `role = "drag"` takes hold of the pointer — the press
arms no text selection and every move until release arrives as a further
`on_click` with `hit.dragging = true`, clamped to the rect it was pressed in.
That is how the terminal pane's scrollbar is grabbable. `hit.w`/`hit.h` carry
the node's size, so a `pure` pane can resolve the coordinate without stashing
geometry in `render`.

**The wheel**: `on_scroll(wheel)` is offered a tick over your pane — `wheel.up`,
plus `wheel.x`/`wheel.y` inside your rect — before the kernel turns it into an
`up`/`down` keystroke for you. Decline it and you keep the keystroke, which is
what a pane that already declares those keys wants. It exists for the pane that
cannot declare them: one taking `input = "session"` hands every unclaimed key to
the agent.

**Speaking and acting outside your pane**: `command("message", { text = …,
level = "info"|"success"|"error" })` puts a sentence in the message band, which
stays kernel-drawn — a pane contributes to it as it contributes a pill or a
binding, rather than spending a row of its own on a message line.
`command("action", { text = "help.open" })` runs a declared action exactly as its
chord or a click on it would, which is how a **key handler** reaches help,
settings, themes or the palette. Before these two, both were reachable only by
painting a node with `role = "action:…"` and waiting for a click.

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

## Names, checked before you run

The other half of the API fails quietly rather than loudly: `convert.rs` drops a
node key it does not know (that is how you carry your own bookkeeping on the node
table), `command` reads a fixed list of option names, and an undefined theme role
is nil. Each of those draws something plausible and reports nothing.

`lib/thurbox.d.lua` is what turns those into findings. Point an editor at the
repository (it reads `.luarc.json`) or run the checker over your own directory:

```bash
lua-language-server --check . --checklevel=Warning
```

No config to write: `lib/thurbox.d.lua` ships beside the panes, and the checker
loads a `---@meta` file that is simply in the directory it is checking.

Two habits make it earn its keep:

- **Annotate the node you build**: `---@type thurbox.TextNode` above a table is
  what lets `txet = "…"` read back as "missing required field `text`". Without
  an annotation there is no type to check against.
- **Name a verb's options as the verb spells them.** `command("open", { url = u })`
  is the classic: the url goes in `text`, and `url` is collected and ignored.

Extra keys in a table are never reported — lua-language-server does not flag
them — which is why each kind and each verb declares the field it cannot work
without, so a misspelling reads as that field missing.

## Traps

These are the ones that cost real time.

- **Asking every frame is correct.** `run(...)` and the `store.want_*` reads are
  designed to be called on every render: a fresh answer is a table lookup, not a
  process. Do not try to call them "only once" — you will get a pane that never
  updates.
- **Measure in columns, not characters.** `#s` is bytes and `utf8.len(s)` is
  codepoints; a terminal budget is neither, and a CJK name is one codepoint over
  two columns. `text.width(s)` is the measure the painter itself uses, and
  `text.truncate` / `text.pad` (or the `widgets` helpers that forward to them)
  spend a budget in it. The exception is `input.cursor`, which is a CHARACTER
  offset — `widgets.chars` is that count.
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
