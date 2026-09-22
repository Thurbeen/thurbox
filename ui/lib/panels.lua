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

return panels
