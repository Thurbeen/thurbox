-- Which side columns are open.
--
-- This exists because of the kernel's second rule: LAYOUT RESOLVES BEFORE
-- RENDER. `layout.lua` has to decide whether the info column gets a column of
-- its own *before* the info plugin runs, so the answer cannot live inside that
-- plugin's render function — it has to be readable from outside it.
--
-- It is kept in `store` rather than in a local table because `store` is the
-- documented bus between plugins AND it survives a hot reload: editing a plugin
-- while the file viewer is open must not slam the column shut. A plain module
-- upvalue would be re-initialised by the reload.
--
-- v1 stored exactly these as `App::show_info_panel` / `show_tasks_panel` /
-- `show_file_viewer` (`src/app/mod.rs`), all three starting closed.

local panels = {}

local KEY = "panels."

--- Panels that start open.
---
--- The session list is the one column v1 shows from the first frame: its F9 is
--- a *hide*, where every other panel key is a show. So the default belongs to
--- the panel rather than to the accessor, and "unset" keeps meaning "whatever
--- this panel starts as".
local OPEN_AT_START = { sessions = true }

--- Declare where one panel starts, for an arrangement whose default differs.
---
--- A preset that starts the list closed (`focus`) or a plugin column open
--- (`ide`) says so here rather than reading "unset" its own way, so the
--- panel's toggle and every pane asking `shown` agree with what is on screen —
--- otherwise a toggle's first press would flip an unset state to the value it
--- already appeared to have, and change nothing. Held in this module rather
--- than in `store`: it is a property of the arrangement loaded now, and a
--- switch of layout rebuilds the VM, so another preset never inherits it.
function panels.starts(name, open)
  OPEN_AT_START[name] = open == true
end

--- Open state of one panel. Unset reads as the panel's start state, which for
--- everything but the session list is closed — v1's.
function panels.shown(name)
  local stored = store[KEY .. name]
  if stored == nil then
    return OPEN_AT_START[name] == true
  end
  return stored == true
end

--- Open one panel, whether or not it already was.
---
--- Distinct from `toggle` because a jump has to *land* somewhere: a search
--- result that focuses a closed column would focus nothing, and toggling would
--- close it half the time. v1 spells the same thing by setting `show_*` directly
--- in `activate_global_search_result` rather than calling its toggle.
function panels.show(name)
  store[KEY .. name] = true
end

--- Close one panel, whether or not it was open.
function panels.hide(name)
  store[KEY .. name] = false
end

--- Flip one panel, returning its new state.
---
--- v1's toggles are independent — opening the file viewer does not close the
--- info panel — so at a wide enough terminal all four columns coexist.
function panels.toggle(name)
  local now = not panels.shown(name)
  store[KEY .. name] = now
  return now
end

--- The filled slots an arrangement does not name — the column an installed
--- pane brings (a file tree, a queue) — sorted, so they keep their order from
--- one frame to the next. `known` is the set the arrangement places itself.
function panels.others(ctx, known)
  local names = {}
  if type(ctx.slots) == "table" then
    for slot, filled in pairs(ctx.slots) do
      if filled == true and not known[slot] then
        names[#names + 1] = slot
      end
    end
  end
  table.sort(names)
  return names
end

--- Did the last arrangement put `slot` on screen?
---
--- Written by the kernel (`placed.<slot>`) after arranging and before any pane
--- renders, because only it knows. What it answers is not the toggle above:
--- a slot can be open and still left out — a narrow screen, or a layout that
--- never names it. The agent pane asks it about `shell` to decide whether its
--- own Shell tab is needed.
function panels.placed(slot)
  return store["placed." .. slot] == true
end

return panels
