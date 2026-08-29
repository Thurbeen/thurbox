# Writing a thurbox plugin

> **The API is settled in its essentials, not frozen.** The four node kinds and
> the snapshot-read/command-write split are load-bearing and asserted by tests.
> What still moves is the published `thurbox.*` shape: a field can be added,
> renamed or dropped in a minor release. `plugins.lock` records the commit each
> installed pane resolved to, so pin what you install and re-run
> `thurbox-cli plugin check` after an upgrade.
>
> **Plugins are trusted code.** They run in-process with whatever capabilities
> the kernel grants, exactly like the shell extensions under `extensions/`.
> Install one the way you would install a shell script from a stranger — which
> is to say, read it first.

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

`plugin new` writes [`examples/lua/plugin.lua`](../examples/lua/plugin.lua) under your
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

**Editing the interface from some other session.** The interface directory is a
config path of yours, so a coding agent working in an unrelated repository has no
reason to know it exists. thurbox handles that for you: the built-in **ui-skill**
extension is on by default and drops one `SKILL.md` (`thurbox-ui`) into each
coding CLI's personal skill directory, so the agent loads the short form of this
page *when* a request is about changing the TUI, in any session. No extra repo
attached to every session, where it would sit in front of the agent whether or
not the work is about the TUI.

```bash
thurbox-cli extension deactivate ui-skill   # not wanted; takes every copy back
thurbox-cli extension activate ui-skill     # and back on
```

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

Ten mistakes that are invisible until runtime. Each cost real time in the
changes that built the bundled panes.

**Declaring `pure` when your render is not.** A pane may declare that its render
is a function of `thurbox.*` and `ctx` and nothing else:

```lua
return {
  name = "notes",
  slot = "left",
  pure = true,   -- the kernel may reuse the tree I last returned
  render = function(ctx) ... end,
}
```

In exchange the kernel stops calling it on frames where nothing it can read has
changed — which is most of them, and is the single largest saving available in a
frame. But nothing checks the claim, and getting it wrong gives you a pane
painted from a stale tree, with no error anywhere. Two things disqualify a pane:

- **It writes `store` or `state` from inside `render`.** Those writes stop
  happening on the frames the render is skipped. This is why the bundled search
  strip is deliberately *not* pure — it leaves its content request in `store`
  while rendering.
- **It animates from `ctx.frame`, or from `ctx.elapsed` faster than the shared
  widgets do.** Animating at the shared rate is fine — the working spinner does,
  and the session list is pure — because the kernel keys a cached tree on that
  same tick. Anything finer freezes.

**Reading `ctx.elapsed` is what buys you the animation tick, and only that.**
The clock advances eight times a second for as long as any session is working,
so it is the most frequent thing a cached tree can be keyed on. The kernel keys
your tree on it *if the render that built the tree read `ctx.elapsed`*, and
otherwise serves that tree across the tick — so a pane that draws no spinner
pays nothing for one, and a pane that draws a spinner keeps it moving. There is
nothing to declare and nothing to get wrong in either direction: read the clock
and you are animated, do not and you are still. Read it under a condition (only
for a `working` row, say) and the render that first reads it is the one that
re-keys the entry, so there is no frame on which a stale tree is served.

The mechanism is a metatable on the render context, which has two visible
consequences: `elapsed` is not a *key* of `ctx`, so it does not appear in
`pairs(ctx)`; and that metatable is sealed — `getmetatable(ctx)` is `false` and
`setmetatable(ctx, …)` raises, because one table is shared by every render and a
plugin replacing it would stop every other pane's clock. Every other field is an
ordinary one. Ask `ctx` for facts by name.

Reading `store`/`state` is fine, and so is `command(...)` from a handler. If you
are unsure, leave it undeclared: a pane that says nothing behaves exactly as it
always has, and the only cost is that it is no faster.

**Reading `state` or `store` hands back a copy.** Mutating it changes nothing —
the value is simply the old one on the next frame. Write the whole thing back:

```lua
local flow = state.flow      -- a fresh table, every read
flow.step = "name"
state.flow = flow            -- without this line, nothing happened
```

**Anything a render computes is gone by the time a key arrives.** A value derived
while drawing lives in a local, and a local is invisible to `on_key` — so share a
*function*, not a field, and what a key acts on is by construction what was on
screen.

`state` is the exception, and deliberately so: writes to it land whenever they
happen, render included. Most panes should not need that — deriving twice is simpler
than remembering — but a decision only a render can make (a click deferred until an
incremental parse reaches the row it named) has nowhere else to go. Prefer the shared
function; reach for `state` when the render is the only place that knows.

**A local used above its definition is `nil`, not an error you can read.** Lua
resolves a `local function` from where it appears. `selene ui` catches it as an
undefined variable; the runtime message will not help you.

**`and`/`or` cannot carry a miss.** `matched = searching and fuzzy(q, row) or {}`
turns a *failed* match (`nil`) into an empty table, which then reads as a match —
so a filter silently keeps everything. Spell it as an `if`.

**A Lua character class is a set of *bytes*.** `text:match("[▸▾]")` does not mean
"either of those arrows" — it means any byte occurring in their encodings, which
matches no arrow and plenty of unrelated things. `#` counts bytes for the same reason,
so a column computed from it comes out short on any row with `é` or `╭` in it. This
interface is full of multi-byte glyphs, so both apply constantly: measure with
`utf8.len`, and compare whole strings rather than classing them.

**`ipairs` stops at the first hole.** It is not "iterate the array"; it is "iterate
until a `nil`". A table built by index where one slot was left empty — a diff row with
no old side, a session with no worktree — ends the loop early and *silently*, and the
symptom is never an error: it is a filter that keeps everything, a count that is short,
or a row reported missing that the screen plainly shows. This has cost real time twice,
once in a bundled pane and once in a plugin written against it. If a table can have a
hole, iterate its indices (`for i = 1, n`) or do not leave one.

**A pane in a `switch` slot needs one key that goes both ways.** `command("focus",
{ text = "<your pane>", toggle = true })` focuses it, and focuses whatever you came
from when it already has focus. Without `toggle` the pane is a one-way door: the key
gets you in and only the focus cycle gets you out. Do not solve that by focusing a
named sibling — the only name available is whatever shares the slot in the *default*
arrangement, which is the user's to change.

**A floating pane needs a slot the arrangement never places.** Otherwise it also
occupies the centre and competes with the terminal. The bundled floats use
`slot = "float"`, which `layout.lua` does not place.

**`on_action` must return `false` while a text field has focus** — and that check
has to be the **first thing it does, for every action**, not a decision made inside
the handler for a particular one. The order is why: a press is resolved against the
registry *before* `on_key` is offered it, so by the time your pane sees the letter
it is already an action. A pane that gates inside `review.refresh` has still
refreshed by the time it notices the find box had focus — the symptom being that
typing a word containing `r` does something, and only that letter.

Every letter your pane declares is a letter somebody will type into a search box.
The flow's `j` moves the list in one focus and types a `j` in another for exactly
this reason.

**A handler that writes `state` on every event repaints on every event.** Right
when the event matters and waste when it does not: check the payload first and
write only what changed. A write that stores the value already there costs
nothing, but a counter that always moves invalidates every pure pane.

**`session.status` fires for the kernel's own derivations too.** A `working`
session that goes quiet for ten seconds reads `idle` (the stuck-working
fallback) and `working` again when it prints; a handler reacting to
`to == "working"` sees both edges.

## Making a pane fast

The trap above says when `pure` is wrong; this is the positive half — the
levers, in the order they pay:

1. **`pure = true`** wherever the render only reads. The kernel reuses the last
   tree until something it read changes and skips your Lua entirely — the
   single largest saving available, and doubly so for floats, which render
   every frame even while closed.
2. **Memoize derived models on published-table identity.** Every gated group
   (`thurbox.sessions`, `thurbox.theme`, `thurbox.registry`, `thurbox.diffs`,
   `thurbox.bookmarks`, …) keeps the *same table object* until its data moves,
   so `rawequal(published, cache.src)` is a sound one-comparison staleness
   test. Build once into an upvalue, rebuild on identity change. Never key a
   cache on anything time-based.
3. **Window before you build.** Compute the visible slice
   (`widgets.window`, or `lib/scroll` for variable-height rows) and build
   spans only for it; a thousand-row list costs its ten visible rows.
4. **Hoist per-row work.** A `store` read crosses the VM boundary; a
   `theme.role` lookup walks tables; `fuzzy.compile(query)` splits the query
   once so per-row matching does not. Read once per render, pass down.
5. **Concatenate through a table.** `s = s .. piece` across a wide row is
   O(width²); accumulate and `table.concat`, or emit `string.rep` runs.
6. **Animate off the shared clock only** — `theme.spinner_frame(ctx.elapsed)`
   follows the kernel's animation tick, which advances only while something is
   animating. A hand-rolled timer re-renders forever and defeats `pure`. And
   read `ctx.elapsed` only where you actually animate: reading it is what
   subscribes your tree to the tick, so hoisting it to the top of a render that
   usually draws nothing moving costs you the cache eight times a second.

`F12` (the perf HUD) is the check: `renders` climbing on an untouched screen
means a pane is not settling. The bundled panes are worked examples — the
session list's memoized model (`lib/session_model.lua`), the flow's row cache,
and the search strip's per-session content memo.

## The directory tells you this too

`AGENTS.md` and `README.md` are delivered into the interface directory itself, so a
session pointed at it has the rules to hand without finding this file. `AGENTS.md`
is the one a coding CLI loads on its own; it is deliberately short and covers what
is easy to get wrong — that "install a plugin" is `thurbox-cli plugin install` and
not a package manager, that `plugin check` gates every edit, and that adding a pane
is two edits. `CLAUDE.md` and `GEMINI.md` beside them point at it rather than
repeating it.

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
| get back a working interface after a bad edit | [When something goes wrong](#when-something-goes-wrong) |

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
| `state` | Private to your plugin. Survives a reload; **not** a restart |
| `store` | Shared by every plugin — the bus between them. Same lifetime as `state` |
| `files.list/read` | Directory entries and file text, rooted at a session's directory |
| `thurbox.settings` | The settings in force: every `[features]` switch, plus the panel breakpoints and scrollback. Read your own switch and decline to draw when it is off — the kernel gates only what it owns |
| `thurbox.bookmarks/browse/branches` | The creation flow's reads: remembered repositories, a directory listing, a base-branch list — each served only while `store.want_bookmarks`/`want_browse`/`want_branches` asks for it |
| `require` | Loads **any** `.lua` under the interface directory, and nothing outside it |

That is worth stating on its own, because it is easy to read `require` as "the
`lib/` the interface shipped": **a pure-Lua library can be vendored into a plugin's
own repository and required from there**, with no kernel change and no capability.
`require("your-pane.vendor.thing")` resolves like any other path, and `plugin
install git+…` is already the delivery mechanism for a repository that carries more
than one file. A Lua tokenizer, a date library, a pretty-printer — none of those
need anything added to the sandbox.

What that does *not* reach is native code. `package` is absent, so there is no
`loadlib` and no C module: anything with a compiled component (tree-sitter, PCRE2)
is out, and would be out even with a grant, because Lua runs on the loop thread and
a blocking call there freezes the frame. `run` and a program pane exist precisely
because they are off the render path by construction.

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

`input` carries one thing the other three do not: a claim on the **caret**. A
screen can hold several fields and the terminal has one cursor, so the field
being typed into says so with `focused = true` and the others say nothing — and
a field that claims it keeps the caret whether or not it holds text, because a
field showing its placeholder is still the field you are typing into.
`lib/textinput.lua` passes its own `focused` option through, so a pane built on
it already does the right thing; a raw `input` node that never sets the flag
draws no caret at all, which is visible at once. A caret guessed from the value
being non-empty is not: it wanders to whichever field happens to hold text.

`surface` is the exception that proves the rule: it carries **cells**, for
content positioned by character measurement rather than by structure — a live
terminal, or a diff body. You place and frame it; the kernel fills it.

The cost of that split, which is easy to meet as a mystery rather than as a fact:
**cells never become nodes, so a surface is invisible to anything that walks the node
tree** — including your own tests. An assertion that looks for text in the tree finds
none of a diff body's content and *matches nothing rather than failing*, which reads
as the feature being broken. Anything asserting on what a surface shows has to go
through the cells you handed it.

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

A pane may open **its own** column and ask for focus in the same action — show the
panel, then `command("focus", { text = name })`, the way `65_search.lua` does. The
slot does not exist until the arrangement runs again, so the kernel holds that
request for one layout and takes it there; you do not have to wait a frame or press
the chord twice.

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

A plugin can also **write** its own settings — `command("set", { text =
"yourpane.wrap", flag = true })` — which is what a view toggle should do. Keeping a
second copy of the value in `state` is the mistake to avoid: both persist, so the
shadow buys nothing and costs the property that matters, because the settings modal
then shows a value your key has silently overridden and resetting it there does
nothing. One home per setting.
Plugin-scoped keys fire only while you have focus, so several panes can all
declare `j`.

Chords are canonicalised, which matters more than it sounds: `shift+j` reaches
you the same way whether the terminal reports a bare `J` or `j` plus SHIFT, and
`ctrl+/` works across all three encodings terminals use for it. `cmd+…` is the
macOS Command key (`super`, `command` and `win` parse as aliases; `cmd` is
canonical), which arrives only from a terminal speaking the kitty keyboard
protocol — `on_key` sees it as `key.cmd` beside `key.ctrl`/`key.alt`/`key.shift`.

A global `ctrl+<letter>` is also a chord the agent's own line editing wants
(`ctrl+r` is reverse-search, `ctrl+d` is EOF). Add `passthrough = true` and a
focused terminal keeps the keystroke while your action stays reachable from
every other pane — which is why the panes that do this also declare an F-key
alternate. It applies only while the bound chord is a bare `ctrl+<letter>`, so a
user who rebinds you onto `f7` gets the action back in the terminal.

`on_key(key)` still exists for panes that need every keystroke — the terminal
uses it, alongside `input = "session"` to forward what it does not handle.

## Events: be told, rather than look

```lua
events = { "session.status", "focus.session" },

on_event = function(name, payload)
  if name == "session.status" and payload.to == "blocked" then
    store.focus_session = payload.session
  end
end,
```

A pane used to learn that the world changed only by being rendered and diffing
the snapshot itself. Declare what you listen for and the kernel calls you **once
per change**, off the render path, with the tables current: a session appeared,
disappeared, changed status, name or branch; the selection or the focused pane
moved; a command a plugin issued finished or failed; the interface reloaded. The
whole list, with each payload, is `thurbox-cli plugin events` and the last
section of `F1`.

The kernel **derives** these by diffing the snapshot, so a session made by
`thurbox-cli`, a cron tick or a second thurbox fires the same `session.created`
as one the creation flow made. The four `session.post_*` names are the exception
and the reason there are two spellings: they fire only for an operation *this*
interface performed, with that operation's facts, and share their names with
`hooks.toml` so shell and Lua learn one vocabulary. Subscribe to
`session.created` to hear about every session; to `session.post_create` to hear
about the one you asked for.

A handler gets exactly what a render gets — the published tables, `state`,
`store`, `command` — and its return value is ignored: it cannot answer, block or
veto, only write state and enqueue. It runs under the render's instruction
budget, and one that throws costs its own subscription for that event: the other
subscribers still run, your pane still draws, and the failure is reported once
per event in the message band rather than painted into your rect every frame.

**A subscription to a name nothing emits refuses to load** (`plugin check` says
which), because a handler that never fires is the one failure with no symptom.

**Plugins can talk to each other.** `command("emit", { text = "refresh", scope =
"x" })` reaches every plugin subscribed to `user.refresh` on the next iteration,
with the other fields as the payload and `payload.source` set to your name — by
the kernel, so nobody can forge it. A kernel name cannot be emitted. Emits may
cascade (a handler emitting to a handler) four generations deep per dispatch;
the fifth is dropped and reported, so two plugins cannot pin the loop between
them.

`examples/lua/events.lua` is the worked example: it selects the session that
just went `blocked`, unless you moved the selection yourself in the last few
seconds.

## The palette: an action without a chord

```lua
commands = {
  { action = "mine.export", desc = "export the list" },
},
```

`Ctrl+P` opens the command palette — every plugin's declared keys, every
`commands` entry, and the kernel's own modals, reload and quit — filtered as you
type, with the chord beside each row that has one. `Enter` runs the chosen row
through the same `on_action` a key press takes, **whether or not your pane is
focused**, so an action must not assume it is (the bundled panes key off `state`
and `store.selected`, never on focus). A command and a key for one action are
one row; a user may later bind a chord to a command from `F1`, at which point it
is a key like any other.

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
{ type = "text", len = 1, role = "url:" .. mr.url,     text = { … } }
```

- `action:<id>` runs a declared action, on whichever plugin declared it — so one
  pane's button can name an action belonging to a pane it has never heard of.
- `key:<chord>` replays the keystroke through the handler the keyboard uses, so
  a button and its letter cannot come to mean different things.
- `focus:<plugin>` focuses that plugin, which in a `switch` slot is also how its
  view is brought forward.
- `url:<link>` opens the link, and is the one verb that also changes how the
  node is *painted* — see below.

### `url:` is a link, not just a click

The value is the whole rest of the role, so a url keeps its own `:` and `//`
(`url:https://example.test/a`, `url:mailto:me@example.test`). A plain click hands
it to the same opener a `Ctrl+Click` on a link in an agent's transcript rides —
including the copy-to-clipboard fallback that carries the url back over ssh — so
a pane's link and a transcript's link cannot open in two different places.

What the other verbs do not do: the node's **drawn cells are re-printed wrapped
in OSC 8**, so `Ctrl+Click` over them is answered by the terminal thurbox itself
runs in. That matters because a pane hands the kernel cells and can emit no
escape of its own, and because on a remote host the outer terminal is the only
leg with a browser to reach. The chord is resolved against `url:` nodes directly
as well, so it still works in an emulator with no OSC 8 support, or on a bare
tty.

Four consequences worth knowing:

- The cells are read back out of the **frame just drawn**, not out of your tree,
  so a node clipped by its pane, covered by a modal or under a float contributes
  no link — the same rule an agent's own runs follow.
- Blank cells are trimmed from either end, because the rect a node was given is
  wider than the glyphs in it and linking the padding would underline the whole
  row. Interior blanks stay, so ` Open MR !123 ` links as `Open MR !123`.
- A node spanning several rows emits one link per row, all naming the same url —
  which is how a wrapped link is spelled in OSC 8 anyway.
- The re-print is written **outside the frame diff**, so it has to spend exactly
  as many columns as the cells it came from. A wide glyph is one cell and two
  columns, and re-printing it moves the cursor over both — so the blank ratatui
  leaves beside it is skipped rather than printed. Getting that wrong is
  permanent, not transient: the next frame repaints only the cells it believes
  moved, so a row shifted by one column stays shifted.

`command("open", { text = url })` is the imperative half, for a link you open
from a keypress rather than a click. The field is `text`, not `url`.

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

**A `surface` can be clicked too, and this is the escape hatch that makes a
geometry-first pane interactive.** A surface carries an `id` like any other node, the
paint walk records the rect of anything carrying identity, and `hit.x` / `hit.y` arrive
*inside* that rect — so a pane resolves a coordinate to whatever it drew there, from the
map it necessarily already has. Cells have no per-line identity and do not need any: the
thing that decided where every row went is the thing that receives the coordinate. That
is what lets a side-by-side diff aim a click at the old or the new column with nothing
added to the node catalog.

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
so it can never trap the user, and except copy and paste, which have to work
from any pane.

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
interface — turning it off is enough to get a working one back, and for a pane you
wrote it is the *only* way back, since `r` has no shipped copy to restore
([When something goes wrong](#when-something-goes-wrong)).

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

## Running a program you interact with

`run` captures a program's output once. It has no stdin and no terminal, so it
cannot give you `htop`, `lazygit`, a REPL or a log you page through. For those a
pane holds a **real terminal**: keystrokes go to the program, it is resized to the
rect, and it keeps running while you work elsewhere.

```lua
capabilities = { "program" },      -- a DIFFERENT capability from `run`
focusable = true,                  -- or it can never be typed at
input = "session",                 -- keys you do not handle go to the surface

render = function(ctx)
  if not (thurbox.granted or {}).program then
    return needs_trust(ctx)        -- absent until you are trusted; draw that
  end
  -- Every frame. Asking for a pane you already have is a map lookup, not a
  -- second copy of the program.
  command("program", { text = "watch", repo = "htop", args = { "-d", "10" } })
  return { type = "surface", program = "watch", fill = 1 }
end,
```

The pane is **yours**, not a session's: one instance whatever is selected. You write
its name and the kernel supplies the owner, so two plugins can both call their pane
`watch` and get two different programs — and neither can name the other's.

`repo` is the program and `args` its arguments, kept separate because the
multiplexer quotes each one: a path with a space in it survives that and would not
survive being concatenated into a command line.

Give one up with `command("program", { text = "watch", action = "close" })`. A
plugin that is removed, renamed or turned off has its panes released for it.

**Why `thurbox.granted` and not `if not program then`.** A capability is normally
withheld by *absence* — that is rule 4, and it is why `run` is simply not a
function until you are trusted. A program pane is asked for through `command`,
which every plugin has, so absence cannot express it: without `granted` a pane
could not tell "you have not trusted me" from "still starting". It grants nothing;
it reports a decision you already made.

**Why it is not `run`'s grant.** `run` is bounded on every axis that matters —
capped output, a timeout, four at a time. An interactive program has none of those
by design, and holds your keyboard as well. Trusting a pane to poll `top` every few
seconds is not the same decision as letting it hold a process open on your
keystrokes, so it is asked separately. The Interface tab says which of the two a
file wants.

**Bounds.** Four panes per plugin — the same number as `run`'s concurrency, so
there is one to remember. `run`'s others do not transfer: an output cap is
meaningless for a screen overwritten in place, and a timeout is the opposite of what
an interactive program wants.

**Lifetime.** Reloading (`F10`) keeps the program running — a reload is an edit to
a file, and losing your editor to one would make reloading unusable. Quitting
leaves it running; the next launch finds it again by its window name, so nothing is
persisted that could go stale. A program that exits on its own is *reported* as
exited rather than drawn as a frozen screen, and asking again starts it afresh.

**Local only, for now.** A plugin's pane has no session and therefore no host, so
it runs on this machine in the interface directory. `run` goes remote because a
session tells it where to; there is nothing here to ask.

## Reserved keys

`ctrl+q` quit · `f10` reload · `ctrl+h` / `ctrl+l` move focus · `f12` perf counters.

These cannot be rebound or consumed, so a misbehaving plugin can never leave the
user stuck inside it.

**`tab` is not among them**, deliberately. Every coding agent uses it for completion,
and a full-screen program in a pane needs it too — an automap, a pager, `vim`. So it
reaches your pane, and a plugin may claim it (the creation flow does). This list used
to name `tab` as moving focus, which was never true and cost a plugin author real
time; focus has only ever moved on `ctrl+h`/`ctrl+l`.

**A disabled plugin reports nothing.** Its keys are free while it is off — another
plugin may claim one, and turning the first back on can then surface a conflict
that did not exist. And a broken one shows no error, because nothing tried to
load it; the error reappears when you turn it on.

**A run is not a stream.** It completes, then reports. Watching `docker logs -f`
is what the shell pane (`Ctrl+T`) is for — it is a real terminal, in the session's
directory, on the session's machine.

**A plugin must handle not being trusted.** The capability is *absent*, not
refusing: `run` is nil, so `command`-style error handling never fires. Check for
it and draw something useful, as `examples/lua/composite.lua` does — that state
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

### Settings → Interface: the way back

`Ctrl+,` (or `F6`), then `]`. It is **chrome, not a pane** — a recovery tool that
was itself a plugin could be the thing that is broken, so nothing you do to the
directory can take it away. It lists every file the interface is made of, what
state that file is in and where it came from; the header line is the directory in
force, which is the answer to "my edits did nothing" — they are usually edits to a
file that is not the one loaded.

**Trouble sorts to the top**, so a failure never sits below thirty healthy rows:

| | State | Means |
|---|---|---|
| `✗` | `failed` | on disk, did not load. Select the row and the **error is in the footer** |
| `⊘` | `removed` | thurbox ships it, you deleted it, delivery has stopped writing it |
| `◌` | `no slot` | it loaded, and `layout.lua` places nothing in its slot — so it never draws |
| `◍` | `off` | present and intact, deliberately not loaded |
| `●` | `on screen` | drawing now |
| `◐` | `on demand` | a float or a modal, at rest |
| `○` | `hidden` | loaded, not currently drawn |
| `·` | | not a pane — `layout.lua`, a `lib/` module |

Four keys act on the selected row:

| Key | Does |
|---|---|
| `r` | **restore** — write the copy thurbox ships back over the file |
| `space` | **off / on** — the file is untouched, simply not loaded |
| `d` | **remove** — deletes. Asked twice, and the confirmation says whether it can be undone |
| `t` | **trust** — grant or withdraw the capabilities the file declares |

Each of the four reloads the interface, so the result is on screen immediately
rather than at the next start, and each says what it did.

### What `r` can put back, and what it cannot

`r` means "put back what thurbox ships, and forget what happened to this file",
which is why it covers both undo cases for a shipped pane — an edit and a
deletion. It has **nothing to put back for a file thurbox never shipped**, and it
says so rather than doing something. So which recovery you have depends on where
the file came from, and the row's own tail is what tells you:

| The row shows | The file is | The way back |
|---|---|---|
| no tail at all | a bundled pane, as shipped | `r`, though there is nothing to undo |
| `edited` | a bundled pane you changed | `r` — the shipped copy is in the binary |
| `removed` | a bundled pane you deleted | `r` |
| `yours` | one you wrote | **`space`**, then fix the file. `r` reports that thurbox ships no version of it |
| `from <src>` | an installed pane | `thurbox-cli plugin sync` — the manager that put it there puts it back. `r` says so and refuses |

A pane you wrote yourself is therefore the one case with no restore, and `space`
is what you reach for: turning it off is enough to get a working interface back
while you fix it, and it is also how you bisect which of several files is at
fault. Remember that **a disabled plugin reports nothing** — nothing tried to
load it, so its error reappears only when you turn it back on.

`layout.lua` and the `lib/` modules are the files whose mistakes cost the whole
screen rather than one pane; both are ordinary rows here, and both are restorable
with `r` while they are the ones thurbox shipped. If the *whole* directory will
not load, the embedded copies are already running — so you are fixing the file
from inside a working interface, not from a blank one.

### The same answers without a terminal

- `thurbox-cli plugin list` — the inventory above, as text.
- `thurbox-cli plugin dir` — the directory in force, and which rule chose it.
- `thurbox-cli plugin check` — loads the interface the way `thurbox` does and
  exits non-zero on a failure, including on a pane that loaded but which no
  arrangement places (it prints the `layout.lua` line to add).

None of the three starts a TUI, which is what makes them the tools for a coding
agent, a CI check, or a terminal you cannot currently trust.

### Two bigger hammers

- **Delete `.bundled.json`** and the next start re-delivers every bundled file,
  forgetting which ones you had removed. Your own files are untouched.
- **Delete the whole directory** for the shipped interface exactly as it ships.

Neither touches `ui.json`, which lives beside the directory rather than in it: a
pane you turned off is still off after both, and a trust you granted is still
recorded. If a decision is what you want to undo, undo it in the Interface tab.

## Examples you can install

Two example panes under `examples/panes/`, neither of them bundled, plus two more under
`examples/lua/` you copy by hand. They exist because "every pane is a file" is
easier to believe from a pane you added yourself than from prose.

**They are examples, not a catalogue.** They are here to be read and copied from,
not a set thurbox maintains on your behalf — installing one is a convenience over
`cp`, and it becomes yours the moment you edit it. For panes meant to be *used*
rather than read, see [Panes that give a v1 surface back](#panes-that-give-a-v1-surface-back)
below.

| Example | What it is |
|---|---|
| `tasks` | v1's tasks pane, rebuilt as a plugin. Reads `thurbox.tasks`, writes `task` commands, needs no capability |
| `top` | CPU, memory and load as gauges, parsed from `top`. Asks for `run`, so it needs your trust |

| Example file | What it is |
|---|---|
| [`plugin.lua`](../examples/lua/plugin.lua) | what `plugin new` writes: a pane, a key, a setting |
| [`composite.lua`](../examples/lua/composite.lua) | the worked `run` example — git status and log, on the session's own host |
| [`events.lua`](../examples/lua/events.lua) | the worked `on_event` example — selects a session the moment it blocks, with a palette command to switch it off |
| [`layout.lua`](../examples/lua/layout.lua) | an arrangement putting the two panes above in a column beside the agent |

Together they are one demo:

```bash
thurbox-cli plugin install tasks
thurbox-cli plugin install top
cp examples/lua/layout.lua ~/.config/thurbox/ui/layout.lua
```

Each install prints the `layout.lua` line the pane needs, because a pane whose slot
nothing places loads cleanly and draws nothing. Press `F10`, then trust
`85_top.lua` (settings → Interface → `t`) so it may run a program — installing a
plugin grants it nothing.

`layout.lua` **replaces** the shipped arrangement — delete yours afterwards and the
Interface tab restores it, so there is nothing here you cannot undo. It is copied
rather than installed on purpose: the manager never writes your arrangement (see
below).

Two things they are chosen to show. `tasks.lua` draws the `input` node kind, which
is the one of the four nothing bundled uses. And `top.lua` reads the machine **the
selected session runs on**, because `run` executes in that session's directory on
that session's host — so moving the cursor onto a session over SSH shows the remote
box's load, with nothing in the plugin knowing what SSH is.

`layout.lua` is the half people forget: adding a pane is two edits, the plugin and
the slot. Both panes name a slot of their own, and a slot no arrangement places is
a pane that loads and never draws. `plugin check` **fails** on exactly that and
prints the line to add, so it is caught rather than puzzled over:

```console
$ thurbox-cli plugin check
  ✓ loads — sessions, agent, confirm, search, new_session, tasks
  ✗ plugins/80_tasks.lua — loaded, but nothing places slot "tasks"
      add it to layout.lua's children: { slot = "tasks" }
  (checked at 200x50)
```

The size is reported because placement depends on it — the shipped arrangement
drops the session column below 80 columns — so "unplaced" only means anything at a
size where the slot should have been placed. Floats need no slot and disabled panes
were never asked for, so neither is reported.

**The quieter sibling: sharing a `switch` slot.** A slot in switch mode shows one
occupant and keeps the rest as alternates, so a pane that is not first draws nothing
until it is focused. Unlike an unplaced slot this fails no check — it loads, it is
placed, `plugin list` says `installed`, and the user's screen is unchanged. It is the
one install that cannot demonstrate itself, and the person it fools is whoever followed
your README.

So **declare a pill**. The action band is kernel chrome and enumerates pills as declared
data, without invoking anything, which makes it the only advertisement that is
automatic — the tab strip beside the agent's views is that *plugin's* own chrome and
cannot carry a third occupant:

```lua
pills = { { action = "mine.open", label = "Mine", priority = 10 } },
```

A low `priority` is right for anything optional: the band drops the least important
entries first when it runs out of width. `plugin check` **warns** about a pane in this
state, and `plugin install` says it at the moment you install one — but it does not fail
either, because you may have meant it.

## Panes that give a v1 surface back

Two of the surfaces v2 dropped are maintained as panes, each in its own repository
rather than in the interface directory thurbox ships. They are not examples: they are
what a plugin looks like when it has to carry v1's behaviour, and they are the answer
to "code review is gone" and "the info panel is gone".

| Pane | What it gives back |
|---|---|
| [`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) | v1's diff reviewer — the branch, a commit or the working changes, a changed-files tree, notes you send back to the agent. Reclaims `Ctrl+X` / `F7` |
| [`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel) | v1's info panel — session, git, agent, usage and system readouts in a column beside the terminal. Reclaims `F2` |

Both carry more than one file, so both install by **cloning** the repository
([A plugin that carries more than Lua](#a-plugin-that-carries-more-than-lua)):

```bash
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-code-review
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-info-panel
```

They differ in the two ways that matter here, which is most of why they are worth
reading:

- **Placement.** The review pane takes the `center` switch slot beside the agent and
  declares a pill, so it needs no `layout.lua` edit and the action band advertises it
  the moment it is installed — the switch-slot problem above, solved the way that
  section says to solve it. The info panel is a column, so it needs its one line in
  `layout.lua`, and its README carries the `max` that stops a label-and-value panel
  being handed a third of a wide screen.
- **Capabilities.** The info panel asks for **nothing**: every readout is in the
  snapshot, including the metrics the kernel gathers on its own workers. The review
  pane asks for `run`, and only for the two targets the kernel does not compute (the
  uncommitted working changes, and a single commit) — untrusted it still draws the
  kernel's `thurbox.diffs`, and the target picker names the choices it cannot serve
  rather than hiding them. That is the shape to copy for an optional capability.

The third missing surface, the file viewer, has no pane — by nobody having written
one rather than by anything withheld: `files.list/read` is published, rooted at a
session's directory.

## Managing panes

Composition is written down, in `plugins.toml` beside your panes:

```toml
# what this interface is made of
[[plugin]]
src  = "top"                    # a bare name, a URL, or a path
file = "plugins/85_top.lua"     # load order lives in the filename
pin  = "v2.1.0"                 # omit to take the newest at install time
```

TOML because that is what every hand-edited registry here is, and because a bad
edit is a parse error naming its line rather than a nil three frames later. A bare
name resolves to `examples/panes/<name>` in the thurbox repository at **this binary's
release tag**, exactly as an extension name resolves to `extensions/<name>` — a
pane reads `thurbox.*`, which is a contract that moves, so what a bare name fetches
matches the binary asking for it. Bare names reach the *examples*; anything you
actually depend on is better named by a URL, a path, or a repository you control.

That tag is also why a bare name can stop resolving: the examples lived under
`ui-plugins/` before they moved to `examples/panes/`, so a `plugins.lock` written
by a binary from before the move records a tag whose tree has no `examples/panes/`
and `plugin sync` reports the pane as not found. Re-run `thurbox-cli plugin install
<name>` to re-lock it at the current tag, or name a URL or a path in
`plugins.toml` instead — which is the same advice as the paragraph above, for the
same reason.

### A plugin that carries more than Lua

A pane that runs a program needs that program, and a pane with data needs the data.
Neither can arrive as text: the file-by-file path returns a `String` and decodes
remote output lossily, so a binary through it is **corrupted rather than refused**.

So a plugin with a payload is a **repository**, and installing it clones it:

```bash
thurbox-cli plugin install git+https://github.com/you/thurbox-widget
```

Everything the repository holds arrives, in the layout you chose — Lua in whatever
directories suit it, a program, a data file. Three forms are recognised as a
repository, all of them explicit: a `git+` prefix, a `.git` suffix, or
`git@host:path`. A bare `https://…` URL deliberately does **not** clone, because
that spelling already means "fetch the manifest's files from this base" and
reinterpreting it by hostname would change what every existing install does.

The working copy lands at `<interface dir>/<name>/`, and **keeps its `.git`**. That
is what makes `update` a fetch rather than a re-download, and it is what protects
your edits: git refuses to move a dirty working tree, so a `sync` over a pane you
changed reports `kept` and leaves it alone. `git diff` shows what you changed and
`git checkout` undoes it — better than any restore we could offer for a file we
never shipped.

That protection cuts both ways, and it is the trap for a pane that *generates*
anything — which is to say, exactly the pane this whole capability invites. A pane that
runs a program is the pane tempted to build one. **Never write inside your own working
copy.** A build artefact, a
downloaded engine, a cache — anything you produce there makes the tree dirty, and a
dirty tree is exactly what makes `update` report `kept` and refuse to move. Keeping
your working copy clean is not tidiness; it is what keeps your plugin updatable. Put
what you generate in `$XDG_CACHE_HOME/<your-plugin>/` (or `~/.cache/<your-plugin>/`),
outside the interface directory entirely — which the next paragraph is also the reason
for.

The spec entry names the pane inside the working copy:

```toml
[[plugin]]
src  = "git+https://github.com/you/thurbox-widget"
file = "thurbox-widget/plugins/40_widget.lua"
```

You rarely write that by hand — `install` finds it, taking the single `.lua` in the
repository's `plugins/` directory, and asks for `--as plugins/<file>` when there is
more than one. Its place in the load order still comes from the `40_` prefix, so
where a plugin came from does not change where it sits. Its own modules are
requirable by path: `require("thurbox-widget.lib.util")`.

The lock records the **commit**, not the branch. `main` moves; a commit does not, so
the same spec and lock reproduce the same bytes on another machine. That is also why
there is no checksum field to maintain: the commit already identifies every byte, and
it is produced by the source rather than transcribed by hand.

`--pin` takes any of the three, and which one you give decides what `update` does:

| `--pin` | what you get | what `update` does |
|---|---|---|
| *(none)* | the default branch's tip | follows it |
| a branch | that branch's tip | follows that branch |
| a tag | the tag | stays |
| a commit | that commit | stays |

A pin is a pin: `update` on a tagged or committed entry reports `current` rather
than moving it, so pinning is how you hold a plugin still. Pinning a commit is what
`--pin` is *for* — it is what the lock writes — and it is worth knowing that `git
clone --branch` cannot do it: a commit is fetched and checked out after the clone.
You get one shallow round trip either way; a pin that cannot be obtained fails with
git's own message and leaves nothing behind.

**Installing a plugin from a repository puts that repository's files on your disk,
executable bits included.** That is what cloning anything does. What it does *not*
do is run any of it: nothing executes at install time, and a program still needs the
`program` capability you grant per file. Treat a repository you did not write the way
you would treat one you were about to `make` in.

**Picking the right build.** `thurbox.platform` gives you `os` and `arch`, so a
plugin shipping several binaries chooses for itself:

```lua
local p = thurbox.platform
local exe = thurbox.ui_dir .. "/thurbox-widget/bin/" .. p.os .. "-" .. p.arch .. "/widget"
```

Deliberately not a manifest field. A substitution template states one rule; a pane
that reads its platform states every rule it actually needs — prefer something
already on `PATH`, fall back to a portable build, distinguish a libc variant, or draw
an honest "nothing here for this machine".

**Do not build under the interface directory.** It is watched *recursively*, so an
`npm install` or a compile there fires thousands of events. `.git` is filtered;
a plugin's own build tree is not, and filtering every possible one is not a rule worth
having — the rule is "build somewhere else". The symptom if you ignore this is the
counter-intuitive one: a burst of events keeps the reload debounce rolling forward, so
the interface does not reload too often, it **stops reloading at all** while you are
busy. A pane that fetches or builds its own engine on first run is exactly the case
that tempts you into it.

**Key releases do not reach a program pane.** thurbox asks its own terminal only for
`DISAMBIGUATE_ESCAPE_CODES`, not `REPORT_EVENT_TYPES`, and the loop handles
`KeyEventKind::Press` alone — so a program that distinguishes press from release
(anything using the kitty keyboard protocol's `CSI > 3 u`) sees presses only, and a
held key latches. Nothing a plugin can do about it; it needs a change in the kernel.
Worth knowing before you build a pane whose program wants held keys.

**If you publish one:** shipping a program under a copyleft licence obliges your
repository to carry that program's corresponding source. That is your obligation, not
thurbox's, but the mechanism invites it. URLs and filesystem paths work too,
and a URL ending in `.lua` installs that single file (with `--as` naming where it
lands).

Beside it, `plugins.lock` records what each entry resolved to and the digest of
every file delivered. You hand-edit the spec; nothing hand-edits the lock. Commit
both and the same interface reproduces elsewhere.

```bash
thurbox-cli plugin install <src> [--as FILE] [--pin V]  # and record it
thurbox-cli plugin sync                                 # make the directory match the spec
thurbox-cli plugin update [name]                        # advance a pin, when you ask
thurbox-cli plugin remove <name>                        # file, spec entry and record
thurbox-cli plugin available                            # what installs by bare name
```

`sync` is the one to reach for after editing the spec by hand, and it is the one an
agent wants: **edit one file, run one command, read the exit status.** It installs
what is missing, takes back what you removed from the spec, and leaves everything
else alone — including a pane the spec never listed, which is nobody's to touch.
Running it twice changes nothing.

Three rules it will not break, all of them the ones delivery already follows:

- **An edit is yours.** A managed file you changed is preserved and reported as
  `kept`, never overwritten — even when the source has moved on.
- **A deletion is remembered.** Delete a managed pane and `sync` leaves it deleted.
  That is how you remove one.
- **Your arrangement is yours.** Nothing here writes `layout.lua`. Editing Lua is
  what a coding agent is good at; noticing that a pane silently is not drawing is
  what it cannot do, so the effort went into `check` instead.

`sync` resolves each entry at the version the **lock** recorded, not at whatever is
newest — that is what makes it reproducible. Moving forward is `update`, asked for
explicitly, which reports what moved and from where. An entry the spec *pins* is
already where you said it should be, so `update` leaves it and says `already
current`; moving that one means editing the pin and running `sync`.

### Trust for a pane you did not write

`run` is granted per file, so where a file came from is the question to answer
before granting it. The Interface tab says: a pane from a source reads `from <src>`
rather than `yours`, and a `lib/<name>/` module a package brought is traced to it
too.

The grant itself is recorded against the **source and version** it was made for,
not against the file's contents alone:

| `src@version` | contents | reads as |
|---|---|---|
| matches | match | `trusted` — including a reinstall |
| differs | — | not granted; you are asked again |
| matches | differ | `installed · modified` |

Both halves matter. Recording only the contents would report every ordinary release
as tampering and teach you to dismiss the warning. Recording only `src@version`
would let a source re-tag the same version with something else and keep a
capability you granted to what that version used to be. That last row is the one to
notice — it is the same warning as a local edit, because from outside they are
indistinguishable.

### There is no lazy loading

The Neovim package managers this borrows from make it the headline feature. Here it
would optimise nothing: a disabled plugin is never read, an unplaced pane never
renders, and the bundled set is 19 files. A load scheduler would add machinery and
a new class of "why is my pane missing".
