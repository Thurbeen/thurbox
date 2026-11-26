local pathpicker = {}

function pathpicker.split_typed(typed)
  if typed == "~" then
    return "~", ""
  end
  local dir = typed:match("^(.*)/[^/]*$")
  if dir == "" then
    dir = "/"
  end
  return dir or "~", typed:match("([^/]*)$") or ""
end

function pathpicker.entries(typed, listed)
  local _, prefix = pathpicker.split_typed(typed or "")
  local shown = {}
  for _, entry in ipairs(listed or {}) do
    local hidden = entry.name:sub(1, 1) == "." and prefix:sub(1, 1) ~= "."
    if not hidden and entry.name:sub(1, #prefix) == prefix then
      shown[#shown + 1] = entry
    end
  end
  return shown
end

function pathpicker.suggestion(typed, listed)
  typed = typed or ""
  local _, prefix = pathpicker.split_typed(typed)
  if typed == "" or prefix == "" then
    return ""
  end
  local shown = pathpicker.entries(typed, listed)
  if #shown ~= 1 then
    return ""
  end
  return shown[1].name:sub(#prefix + 1) .. "/"
end

function pathpicker.common_root(rows)
  local common, absolute
  for _, row in ipairs(rows or {}) do
    if not row.is_parent and not row.offered and row.path then
      local parts = {}
      for part in row.path:gmatch("[^/]+") do
        parts[#parts + 1] = part
      end
      parts[#parts] = nil
      local rooted = row.path:sub(1, 1) == "/"
      if not common then
        common, absolute = parts, rooted
      elseif rooted ~= absolute then
        return "~"
      else
        local shared = 0
        while shared < #common and common[shared + 1] == parts[shared + 1] do
          shared = shared + 1
        end
        for index = #common, shared + 1, -1 do
          common[index] = nil
        end
      end
    end
  end
  if not common then
    return "~"
  end
  local joined = table.concat(common, "/")
  if absolute then
    return "/" .. joined
  end
  return joined ~= "" and joined or "~"
end

function pathpicker.start(rows)
  local root = pathpicker.common_root(rows)
  return root == "/" and "/" or (root .. "/")
end

return pathpicker
