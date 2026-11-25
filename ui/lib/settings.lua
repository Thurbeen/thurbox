local settings = {}

local function all()
  return (thurbox and thurbox.registry and thurbox.registry.settings) or {}
end

function settings.get(plugin, id, fallback)
  for _, entry in ipairs(all()) do
    if entry.mod == plugin and entry.id == id then
      if entry.value ~= nil then
        return entry.value
      end
      return fallback
    end
  end
  return fallback
end

function settings.enabled(plugin, id, fallback)
  local value = settings.get(plugin, id, fallback)
  if value == nil then
    return fallback == true
  end
  return value == true
end

function settings.feature(name, fallback)
  local published = thurbox and thurbox.settings
  local features = published and published.features
  if not features or features[name] == nil then
    return fallback
  end
  return features[name]
end

return settings
