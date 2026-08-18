-- Doom, in a pane.
--
-- NOT bundled. Install it into your interface with:
--
--     thurbox-cli plugin install doom
--
-- The point is not the game. It is that a pane can hold a **real terminal** for a
-- program you name — keystrokes go to it, it is resized to the rect, and it keeps
-- running while you work elsewhere. `run` cannot do this: it captures output once,
-- with no input and no tty. If doom works, so do `htop`, `lazygit`, a REPL, and a
-- log you page through.
--
-- To use it, install it and then trust it: settings (`Ctrl+,`) → `]` → find it →
-- `t`. Until you do, `thurbox.granted.program` is false here and the pane says so.
-- That is not an error state; it is the state every plugin starts in, and drawing
-- it well is part of the job.
--
-- WHY `granted` AND NOT `if not run then`. A capability is normally withheld by
-- ABSENCE — `run` is simply not a function until you are trusted, which is the
-- check the `composite` example makes. A program pane is asked for through
-- `command`, which every plugin has, so absence cannot express it. `granted` is how
-- the kernel answers instead, and it grants nothing: it reports a decision you
-- already made.
--
-- THE ONE RULE THAT MATTERS, same as `run`'s. Ask on every frame. Asking for a
-- pane you already have is a map lookup, not a second copy of the program — so
-- there is no clever place to "start it once", and no state of your own to keep.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

local NAME = "doom"

--- The pane this plugin owns. One name, one program.
---
--- Scoped to this plugin by the kernel: you write the name, it supplies the
--- owner. Two plugins can both call their pane `doom` and get two different
--- programs — you cannot name somebody else's.
local PANE = "doom"

--- What to run, and what to run it with.
---
--- Separate rather than one command line because the multiplexer quotes each
--- argument: a WAD path with a space in it survives this and would not survive
--- being concatenated.
local PROGRAM = "doom"
local ARGS = {}

--- Have we been trusted with the capability this pane needs?
local function granted()
  local grants = (thurbox and thurbox.granted) or {}
  return grants.program == true
end

--- The pane, before it has been trusted.
---
--- Drawn rather than left blank on purpose: an empty box is indistinguishable from
--- a broken plugin, and the fix is two keystrokes away in a modal the reader may
--- not know exists.
local function untrusted(ctx)
  -- `len = 1` on each line, and one filler at the end. Without a size, every child
  -- of a box takes an equal share of it — which for six lines of prose in a tall
  -- pane spreads them down the screen with gaps between.
  local function line(text)
    return { type = "text", text = theme.dim(text), wrap = true, len = 1 }
  end
  return {
    type = "box",
    frame = widgets.panel(" Doom ", ctx.focused),
    children = {
      line(""),
      line("  this pane wants to run a program you interact with."),
      line(""),
      -- Deliberately NOT naming this file. `--as` can put it anywhere, so a
      -- hardcoded name would be a confident lie; the tab lists what asked.
      line("  trust it: settings (ctrl+,) → ] → find this file → t"),
      line(""),
      line("  granted per file, and separately from `run`: a program you"),
      line("  type at is not the same as a program you read the output of."),
      { type = "text", text = "", fill = 1 },
    },
  }
end

return {
  name = NAME,
  -- A slot of its own. `layout.lua` has to place it, or this loads and draws
  -- nothing — `thurbox-cli plugin check` fails on exactly that and prints the line
  -- to add.
  slot = "doom",
  -- Reachable in the `ctrl+h`/`ctrl+l` focus ring. Without this the pane draws
  -- perfectly and can never be typed at, since raw input only goes to the plugin
  -- that HAS focus — an easy thing to leave out and a confusing thing to debug.
  focusable = true,
  -- Keys this pane does not handle go to whatever its surface names, which for a
  -- game is all of them. The escape chords (`ctrl+h`/`ctrl+l`, `ctrl+q`) are never
  -- forwarded, so a program that eats every key cannot trap you in it.
  input = "session",
  capabilities = { "program" },

  render = function(ctx)
    if not granted() then
      return untrusted(ctx)
    end

    -- Every frame. Idempotent by design; see the note at the top.
    command("program", { text = PANE, repo = PROGRAM, args = ARGS })

    return {
      type = "box",
      frame = widgets.panel(" Doom ", ctx.focused),
      children = {
        -- The surface resolves to this plugin's own pane. The kernel reports what
        -- to draw when there is nothing behind it yet, and says so when the
        -- program has exited — a frozen grid would look exactly like a live one.
        { type = "surface", program = PANE, fill = 1 },
      },
    }
  end,

  keys = {
    {
      key = "ctrl+alt+d",
      action = "doom.close",
      desc = "close the doom pane",
      group = "Doom",
    },
  },

  on_action = function(action)
    if action == "doom.close" then
      -- Giving it up deliberately, rather than leaving it running unseen. A
      -- plugin that is removed or turned off has its panes released for it.
      command("program", { text = PANE, action = "close" })
      return true
    end
    return false
  end,
}
