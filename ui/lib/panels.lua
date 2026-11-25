local panels = {}

local KEY = "panels."

local OPEN_AT_START = { sessions = true }

function panels.shown(name)
  local stored = store[KEY .. name]
  if stored == nil then
    return OPEN_AT_START[name] == true
  end
  return stored == true
end

function panels.show(name)
  store[KEY .. name] = true
end

function panels.hide(name)
  store[KEY .. name] = false
end

function panels.toggle(name)
  local now = not panels.shown(name)
  store[KEY .. name] = now
  return now
end

return panels
