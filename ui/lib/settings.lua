-- Reading back what your plugin declared.
--
-- A plugin declares settings as data:
--
--   settings = {
--     { id = "group_by_repo", desc = "Group sessions under a repo header", default = true },
--   },
--
-- and the kernel collects every plugin's declarations into one registry, which
-- the settings modal renders. That is the whole extension point: you get a row
-- in settings by declaring a field, and the modal never learns what it is for.
--
-- The registry is published FLAT -- every plugin's settings in one list -- so
-- reading your own means filtering it. This module is that filter, so a plugin
-- reads a knob in one line instead of repeating the search.

local settings = {}

local function all()
  return (thurbox and thurbox.registry and thurbox.registry.settings) or {}
end

--- The effective value of `plugin`'s `id`, or `fallback` when it is unset.
---
--- `value` is what the registry resolved: the user's override when there is
--- one, otherwise the declared default. A plugin therefore never needs to know
--- whether a setting was overridden, only what it currently is.
function settings.get(plugin, id, fallback)
  for _, entry in ipairs(all()) do
    if entry.plugin == plugin and entry.id == id then
      if entry.value ~= nil then
        return entry.value
      end
      return fallback
    end
  end
  return fallback
end

--- `settings.get` narrowed to booleans, since most knobs are toggles and a
--- caller wants a definite true/false rather than nil.
function settings.enabled(plugin, id, fallback)
  local value = settings.get(plugin, id, fallback)
  if value == nil then
    return fallback == true
  end
  return value == true
end

return settings
