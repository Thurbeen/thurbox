# Writing a thurbox plugin

> **The API is public but unstable.** It will change without a deprecation
> period until it settles. Plugins are **trusted code**: they run in-process
> with whatever capabilities the kernel grants, exactly like the shell
> extensions under `extensions/`. Install one the way you would install a shell
> script from a stranger — which is to say, read it first.

A plugin is one `.lua` file. Drop it in your plugin directory and it loads on the
next save; there is no build step and no restart.

## Start here

Four commands, and you have a pane on screen. None of them need the interface to
be running.

```bash
thurbox-cli plugin dir          # where plugins live, and which rule chose it
thurbox-cli plugin new notes    # a starter that already loads
thurbox-cli plugin check        # does it load? exits non-zero if not
thurbox-cli plugin list         # every file, where it came from, is it drawn
```

`plugin new` writes [`docs/examples/plugin.lua`](examples/plugin.lua) under your
chosen name — a pane that renders, declares a key and a setting, and comments the
one rule that catches everybody (see **Traps**). Edit it, run `check`, and it is
live on the next save: the interface watches the directory.

**Which directory?** Two rules — `THURBOX_UI_DIR` if it is set, otherwise your own
copy at `~/.config/thurbox/ui`. `plugin dir` reports the one in force *and why*,
because "my edit did nothing" is almost always the other one.

That third path follows the same rule every other config path does: a **dev
build** (version `0.0.0-dev`) reads `~/.config/thurbox-dev/ui` instead, beside its
own `settings.toml` and `agents.toml`, so hacking on the interface from a checkout
cannot touch the copy your installed thurbox uses. Two things make this easy to
misread, and both are worth knowing before you conclude a path is wrong:

- **`THURBOX_CONFIG_DIR` wins over both**, and thurbox *injects it into every
  session it spawns*. So a `thurbox-cli` run from inside a thurbox session
  resolves against **that session's** config dir, not the dev default — a dev
  binary invoked there will correctly report the release `ui` directory.
- **Standing in a checkout changes nothing.** A `./ui` beside the working directory
  used to win automatically, which is what made the interface the one config that
  ignored the dev/release split. To work on a checkout's interface, ask for it:
  `THURBOX_UI_DIR=ui`, or `just tui-ui` in the repository.

`thurbox-cli config show` prints the resolved `ui_dir` and `ui_json` alongside
every other config path, which is the quickest way to see which set is in play.

```text
<the directory plugin dir reports>/
  layout.lua                   how the screen is arranged
  lib/                         shared modules, via require("lib.theme")
  plugins/*.lua                one file per pane
```

Inside a running interface, the **settings modal's Interface tab** shows the same
inventory `plugin list` does, and restores a file you broke. Open settings with
`Ctrl+,` or `F6`, then `[` / `]` to move between its tabs — or click a heading.

## Traps

Five mistakes that are invisible until runtime. Each cost real time in the
changes that built the bundled panes.

**Reading `state` or `store` hands back a copy.** Mutating it changes nothing —
the value is simply the old one on the next frame. Write the whole thing back:

```lua
local flow = state.flow      -- a fresh table, every read
flow.step = "name"
state.flow = flow            -- without this line, nothing happened
```

**Anything a render computes is gone by the time a key arrives.** `render` does
not write `state`, so a value derived while drawing is invisible to `on_key`.
Share a function, not a field — then what a key acts on is by construction what
was on screen.

**A local used above its definition is `nil`, not an error you can read.** Lua
resolves a `local function` from where it appears. `selene ui` catches it as an
undefined variable; the runtime message will not help you.

**`and`/`or` cannot carry a miss.** `matched = searching and fuzzy(q, row) or {}`
turns a *failed* match (`nil`) into an empty table, which then reads as a match —
so a filter silently keeps everything. Spell it as an `if`.

**A floating pane needs a slot the arrangement never places.** Otherwise it also
occupies the centre and competes with the terminal. The bundled floats use
`slot = "float"`, which `layout.lua` does not place.

**`on_action` must return `false` while a text field has focus**, or your pane's
own letter keys swallow typing. The flow's `j` moves the list in one focus and
types a `j` in another for exactly this reason.

## Your copy of the interface

The bundled panes are not special files. They are written into your directory on
first run and read back from it, so the interface you were shipped is the
interface you edit — there is no second, privileged copy running underneath.

| You want to | Do this |
|---|---|
| change a bundled pane | edit the file; it reloads on save and is never overwritten again |
| add one | drop a `.lua` file in `plugins/` |
| replace one | write your own and **delete** the bundled file |
| remove one | delete the file |
| undo either | settings → Interface, select it, `r` |
| turn one off | settings → Interface, select it, `space` |

**Deleting is how you remove.** A file thurbox wrote and can no longer find is
recorded as removed and is not written again — not on the next start, and not by
an upgrade that changes it. Nothing is lost by it: the shipped copy is in the
binary, so `r` in the Interface tab puts it back, and the same key discards your edits
to a file you would rather have back as it shipped.

What an upgrade does to each file follows from the same record:

- **untouched** → updated, so fixes reach you;
- **edited** → left alone, and reported;
- **removed** → left removed;
- **yours** → not touched, ever. Delivery writes only files it ships;
- **no longer shipped** → taken back if you never changed it, kept if you did.

Removing every pane is allowed and does what it says: nothing draws, the chrome
still works, and the Interface tab still lists what you removed. It is not treated as a broken
directory.

## The smallest plugin

```lua
return {
  name = "hello",
  slot = "center",
  focusable = true,

  render = function(ctx)
    return { type = "text", text = "hello from " .. ctx.width .. " columns" }
  end,
}
```

`render` returns a plain table describing what to draw. **Lua never holds a
ratatui object** — that indirection is why reloading is safe, and why a plugin
that throws costs its own pane and nothing else.

## What you get

| Global | What it is |
|---|---|
| `thurbox` | Everything readable: sessions, tasks, automations, repos, agents, hosts, theme, registry, diffs, links |
| `command(kind, opts)` | The only way to change anything. Enqueues and returns |
| `state` | Persistent, private to your plugin. Survives reload |
| `store` | Persistent, shared by every plugin. The bus between them |
| `files.list/read` | Directory entries and file text, rooted at a session's directory |
| `thurbox.settings` | The settings in force: every `[features]` switch, plus the panel breakpoints and scrollback. Read your own switch and decline to draw when it is off — the kernel gates only what it owns |
| `thurbox.bookmarks/browse/branches` | The creation flow's reads: remembered repositories, a directory listing, a base-branch list — each served only while `store.want_bookmarks`/`want_browse`/`want_branches` asks for it |
| `require` | Loads `lib/*.lua`, and nothing outside the plugin directory |

**There is no filesystem, no process spawning and no network.** Not because they
are blocked — because they are not there. A capability you were not granted is
absent from the environment, so there is nothing to call. `io`, `os`, `debug`,
`dofile`, `loadfile`, `print` and `warn` are all withheld.

You will hear about it before you run it. `selene ui` checks `ui/` against
`thurbox.yml`, and `lua-language-server --check ui` against `.luarc.json` — between
them every withheld capability is declared absent, so reaching for one is a lint
error rather than a nil-index at the moment someone opens your pane. `stylua ui`
formats. All three run in CI; selene and stylua also run on commit.

## The four node kinds

`text`, `box`, `input`, `surface`. That is the whole vocabulary, and it is meant
to stay that way.

Lists, gauges, panels, dividers and tables are **not** node kinds — they are
`lib/widgets.lua`, composed from the four. When you need a new appearance, add
it there. A prior version of this design froze its catalog at six kinds and
watched it reach sixteen, because every new appearance had nowhere else to go
and each one cost a release. A widget in Lua costs a file save.

```lua
local widgets = require("lib.widgets")

widgets.list({ rows = rows, selected = 3, height = ctx.height - 2 })
widgets.panel("title", ctx.focused)
widgets.gauge(0.7, { width = 20 })
```

`surface` is the exception that proves the rule: it carries **cells**, for
content positioned by character measurement rather than by structure — a live
terminal, or a diff body. You place and frame it; the kernel fills it.

## Sizing, and why your rect is known

Every child of a `box` says how much of the axis it wants:

```lua
{ len = 3 }      -- exactly three
{ pct = 50 }     -- half
{ fill = 2 }     -- twice the share of a fill = 1 sibling
{ min = 5, max = 20 }
```

`ctx.width` and `ctx.height` are **your pane's**, not the screen's. The kernel
resolves every rect before calling you, which is what makes wrapping,
truncation and scroll windows possible at all.

## Slots: where your pane lands

`slot` names the region of `layout.lua` your plugin draws into. The stock
arrangement is two columns:

| Slot | Where | Shown when |
|---|---|---|
| `sessions` | far left | width ≥ 80 **and** toggled open (F9) |
| `center` | the remainder | always |

A pane that only ever floats — the new-session flow is the bundled example —
names a slot nothing places, so it never competes for the centre.

A slot exists because a plugin fills it, not the other way round: v1's `info`,
`tasks` and `files` columns and its header/footer bands were removed with their
plugins, and their slots went too — a slot nothing can fill would reserve a rect
for nothing. **Adding a pane means adding its slot to `layout.lua`**, which is a
file you edit rather than a layout compiled into the binary.

Several plugins may name the same slot. `center` is a **switch** slot — one
occupant is visible at a time and focusing one brings it forward, so the focus
ring visits every occupant and moving onto one selects it. A slot the arrangement
did not place this frame simply does not draw, and focus skips its plugins, so a
closed column can never hold focus.

The distinction between "not drawn" and "cannot hold focus" is load-bearing:
focusing a switch alternate is *what makes it drawn*, so the two questions have
separate answers in `kernel::focus`. Conflating them cost the old plugins pane every
way in it had — the ring skipped it, and its own opening chord was undone a frame
later by the guard that keeps focus off closed columns.

Because layout resolves *before* render, anything the arrangement needs to know
cannot live inside a render function. Panel visibility therefore lives in
`lib/panels.lua`, which keeps it in `store` so it survives a reload:

```lua
local panels = require("lib.panels")

panels.shown("sessions")   -- is the column open?
panels.toggle("sessions")  -- flip it, returns the new state
```

## Colour: name roles, never values

```lua
local theme = require("lib.theme")
{ text = "hi", style = { fg = theme.accent } }     -- yes
{ text = "hi", style = { fg = "#5fafff" } }        -- no
```

The active theme resolves roles to colours. Naming a role means your pane
follows every one of the 36 built-in themes and any the user wrote, without you
knowing they exist. Hardcoding a colour opts out of that for everyone
downstream — and there is a test that greps the bundled plugins for literals.

## Keys: declare them, don't just handle them

```lua
keys = {
  { key = "j", action = "mine.next", desc = "next item" },
  { key = "ctrl+n", action = "mine.new", desc = "new", scope = "global" },
},

on_action = function(action)
  if action == "mine.next" then ... return true end
  return false
end,
```

Declaring keys as data is what lets the kernel list them in help, detect a clash
with another plugin, and let the user rebind them — none of which it could do if
they only existed inside `on_key`. Help is a kernel modal rather than a plugin,
and it renders the registry, so your key appears in it (and becomes rebindable)
by being declared and nothing else. The same is true of `settings`: declare
`{ id, desc, default }` and the settings modal grows a row for it.
Plugin-scoped keys fire only while you have focus, so several panes can all
declare `j`.

Chords are canonicalised, which matters more than it sounds: `shift+j` reaches
you the same way whether the terminal reports a bare `J` or `j` plus SHIFT, and
`ctrl+/` works across all three encodings terminals use for it.

A global `ctrl+<letter>` is also a chord the agent's own line editing wants
(`ctrl+r` is reverse-search, `ctrl+d` is EOF). Add `passthrough = true` and a
focused terminal keeps the keystroke while your action stays reachable from
every other pane — which is why the panes that do this also declare an F-key
alternate. It applies only while the bound chord is a bare `ctrl+<letter>`, so a
user who rebinds you onto `f7` gets the action back in the terminal.

`on_key(key)` still exists for panes that need every keystroke — the terminal
uses it, alongside `input = "session"` to forward what it does not handle.

## Clicks: give the node an identity

The kernel hit-tests the tree it just painted, so a node becomes a click target
by carrying identity — the same `id` / `class` / `role` a decorator matches on.
Nothing else is declared, and there is no new node kind.

For the cases where a click should do exactly what a key does, `role` names the
verb and the kernel answers it without calling you at all:

```lua
{ type = "text", len = 8, role = "action:themes.open", text = " Theme " }
{ type = "text", len = 5, role = "key:ctrl+q",         text = " Quit " }
{ type = "text", len = 7, role = "focus:notes",        text = " Notes " }
```

- `action:<id>` runs a declared action, on whichever plugin declared it — so one
  pane's button can name an action belonging to a pane it has never heard of.
- `key:<chord>` replays the keystroke through the handler the keyboard uses, so
  a button and its letter cannot come to mean different things.
- `focus:<plugin>` focuses that plugin, which in a `switch` slot is also how its
  view is brought forward.

Everything else — a list row, most often — is offered to the plugin that painted
it:

```lua
on_click = function(hit)
  -- hit.id, hit.class, hit.role, plus hit.x / hit.y inside the node's own rect
  if not hit.id then return false end
  state.cursor = index_of(hit.id)
  return true
end,
```

Return `false` and the press falls through, which is what lets the same click
that focused a terminal also start a drag-selection over it. A pane needs no
`on_click` to be clickable: any click focuses the pane it lands in first.

Rows built by `widgets.list` already carry `role = "row"` and whatever `id` you
gave them, so a list is clickable as soon as it has an `on_click`.

Everything here is inert when `[features] mouse` is off — the capture escape is
never sent, so the terminal keeps its own selection and scrolling.

## Changing things

```lua
command("delete",  { session = id })
command("create",  { repo = "/src/thing", branch = "feat/x", agent = "claude" })
command("task",    { number = 3, status = "done" })
command("theme",   { text = "tokyo-night" })
```

**Commands never block and never return a result.** They are accepted instantly
and their effect appears in a later snapshot. Work in flight is readable at
`thurbox.commands`, so you can draw it rather than leaving an unexplained gap:

```lua
for _, item in ipairs(thurbox.commands) do
  -- item.kind, item.session, item.subject, item.phase, item.error
end
```

This is not a limitation to work around. A plugin that could wait would be a
plugin that can freeze the interface on a slow git fetch or an unreachable SSH
host.

## Floating panes and modals

```lua
floats = true,   -- declared once: this pane may float

render = function(ctx)
  if not state.open then return { type = "text", text = "" } end
  return { float = { width = 50, height = 30 }, type = "box", ... }
end,
```

`width`/`height` are percentages of the screen; `cols`/`rows` ask in cells and
win where both are given. A modal framing a list of a known length wants its
height in rows — sized by percentage its frame drifts away from its content as
the terminal grows.

```lua
{ float = { width = 60, rows = 18 }, ... }   -- v1's modals: 60% wide, height to fit
```

A modal is *open* on the frames it returns a `float` node, and closed on the
ones it does not. There is no open/close state for the kernel and your plugin to
disagree about. While it floats it takes every key — except the reserved ones,
so it can never trap the user.

## Decorating another pane

```lua
decorates = "sessions",

decorate = function(tree)
  local t = require("lib.tree")
  return t.restyle(tree, function(node) return node.role == "row" end,
                   { fg = theme.accent, bold = true })
end,
```

You receive another plugin's rendered tree and return a modified one, matching
on the `id`/`class`/`role` nodes carry. This is how search highlights matches
inside panes it does not own. A decorator that throws costs its decoration, not
the pane.

## Turning a plugin off

`space` in the Interface tab turns the selected plugin off, and on again. The
file is not touched: it stays exactly where it is and is simply not loaded. That
is the thing to reach for when you want a pane gone for an afternoon, when you
are bisecting a problem, or when a plugin you are writing has broken the
interface — turning it off is enough to get a working one back.

**`d` is not that.** It deletes. For a file thurbox ships that is recoverable
(`r` writes the shipped copy back); for **a file you wrote there is no copy**,
and the removal is permanent. The confirmation says which one you are about to
do — read it.

A disabled plugin is genuinely absent, not dormant: it declares no keys, offers
no settings, occupies no slot and is granted no capability. Two things follow
that are worth knowing. Its key is **free** while it is off, so another plugin
may claim it — and turning the first one back on can then surface a conflict that
did not exist. And a **broken** disabled plugin reports nothing, because nothing
tried to load it; its error reappears when you turn it on.

## Running a program

A pane over `git status`, `docker compose ps` or `npm outdated` needs to *run*
those, and Lua here has no process, no filesystem and no network. `run` is the
one door, and it opens only for a plugin that asks for it and that you have
trusted.

```lua
capabilities = { "run" },          -- declare it, or `run` is not a function

render = function(ctx)
  if not run then                 -- absent until you are trusted; draw that
    return needs_trust(ctx)
  end
  run("status", "git status --porcelain", { session = id, ttl = 2 })
  local got = (thurbox.runs or {}).status
  if got and got.state == "done" and got.ok then
    -- got.stdout, got.stderr, got.status, got.truncated, got.timed_out
  end
end,
```

**Ask on every frame.** `run` does nothing while the answer is fresh *or while a
run for that key is already going*, so asking is a map lookup rather than a
process — and asking once, somewhere clever, is how a pane ends up showing
yesterday's answer forever. The in-flight half matters for a program slower than
its own `ttl`: without it every frame after the answer went stale would start
another copy. `refresh = true` overrides freshness; that is what a "reload" key
does.

**Trusting a plugin** is settings (`Ctrl+,`) → `]` → select it → `t`. The
Interface tab shows which files ask to run programs, which are trusted, and
whether a trusted file has changed since you trusted it. Revoking takes effect on
the next frame.

This is not a sandbox and does not pretend to be one. A program thurbox runs for
you has your authority, and no gate here changes that — what trust buys is that
nothing runs *unasked*, per plugin, revocably. Treat a plugin you did not write
the way you would treat a shell profile someone sent you.

**The kernel's bounds**, which a plugin cannot raise: output is capped per stream
and flagged when truncated, a run times out (30 s by default, `timeout = n` up to
ten minutes), and four run at once with the rest queued. A run happens in the
session's working directory, and for a remote session **on that session's host** —
which is what makes `docker compose ps` mean the right containers.

## Reserved keys

`ctrl+q` quit · `f10` reload · `tab` / `shift+tab` and `ctrl+h` / `ctrl+l` move
focus · `f12` perf counters.

These cannot be rebound or consumed, so a misbehaving plugin can never leave the
user stuck inside it.

**A disabled plugin reports nothing.** Its keys are free while it is off — another
plugin may claim one, and turning the first back on can then surface a conflict
that did not exist. And a broken one shows no error, because nothing tried to
load it; the error reappears when you turn it on.

**A run is not a stream.** It completes, then reports. Watching `docker logs -f`
is what the shell pane (`Ctrl+T`) is for — it is a real terminal, in the session's
directory, on the session's machine.

**A plugin must handle not being trusted.** The capability is *absent*, not
refusing: `run` is nil, so `command`-style error handling never fires. Check for
it and draw something useful, as `docs/examples/composite.lua` does — that state
is the first thing every user of your plugin will see.

## When something goes wrong

- A plugin that fails to load leaves the **last good version running**, with the
  error on screen.
- A plugin that throws while rendering shows a red panel **in its own rect**.
- A plugin that never returns is **interrupted** — an unterminated loop costs one
  pane, not the application.
- A plugin that allocates without bound hits a **memory ceiling** and fails.
- If your whole plugin directory will not load, the **embedded copies** run
  instead, so you can fix the file from inside the thing it broke.

Three escape hatches, in increasing order of how much you give up:

- **Settings → Interface** lists every file with its state, and restores one at a time. It also
  names the directory in use — if your edits appear to do nothing, they are
  probably to a file that is not the one loaded (a `./ui` beside the working
  directory is only used when `THURBOX_UI_DIR` names it). Outside the interface,
  `thurbox-cli plugin list` and `thurbox-cli plugin dir` answer the same two
  questions, and `thurbox-cli plugin check` reports a load failure without
  starting anything.
- **Delete `.bundled.json`** and the next start re-delivers every bundled file,
  forgetting which ones you had removed. Your own files are untouched.
- **Delete the whole directory** for the shipped interface exactly as it ships.

## Examples you can run

Four files under `docs/examples/`, none of them bundled — copy the ones you want.
They exist because "every pane is a file" is easier to believe from a pane you
added yourself than from prose.

| File | What it is |
|---|---|
| [`plugin.lua`](examples/plugin.lua) | what `plugin new` writes: a pane, a key, a setting |
| [`composite.lua`](examples/composite.lua) | the worked `run` example — git status and log, on the session's own host |
| [`tasks.lua`](examples/tasks.lua) | v1's tasks pane, rebuilt as a plugin. Reads `thurbox.tasks`, writes `task` commands, needs no capability |
| [`top.lua`](examples/top.lua) | CPU, memory and load as gauges, parsed from `top`. Asks for `run`, so it needs your trust |
| [`layout.lua`](examples/layout.lua) | an arrangement putting the two above in a column beside the agent |

The last three are one demo:

```bash
cp docs/examples/layout.lua ~/.config/thurbox/ui/layout.lua
cp docs/examples/tasks.lua  ~/.config/thurbox/ui/plugins/80_tasks.lua
cp docs/examples/top.lua    ~/.config/thurbox/ui/plugins/85_top.lua
```

Press `F10`, then trust `85_top.lua` (settings → Interface → `t`) so it may run a
program. `layout.lua` **replaces** the shipped arrangement — delete yours
afterwards and the Interface tab restores it, so there is nothing here you cannot
undo.

Two things they are chosen to show. `tasks.lua` draws the `input` node kind, which
is the one of the four nothing bundled uses. And `top.lua` reads the machine **the
selected session runs on**, because `run` executes in that session's directory on
that session's host — so moving the cursor onto a session over SSH shows the remote
box's load, with nothing in the plugin knowing what SSH is.

`layout.lua` is the half people forget: adding a pane is two edits, the plugin and
the slot. Both example panes name a slot of their own, and a slot no arrangement
places is a pane that loads and never draws — `plugin list` reports it as
`no slot`, which is the fastest way to spot it.
