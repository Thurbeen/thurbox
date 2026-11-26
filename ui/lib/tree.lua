local tree = {}

function tree.has_class(node, class)
  if type(node.class) ~= "string" then
    return false
  end
  for word in string.gmatch(node.class, "%S+") do
    if word == class then
      return true
    end
  end
  return false
end

function tree.text_of(node)
  local out = {}
  local function walk(value)
    if type(value) == "string" then
      out[#out + 1] = value
    elseif type(value) == "table" then
      if value.text then
        walk(value.text)
      end
      for _, item in ipairs(value) do
        walk(item)
      end
    end
  end
  walk(node.text)
  return table.concat(out)
end

function tree.map(node, fn)
  if type(node) ~= "table" then
    return node
  end
  local replaced = fn(node) or node
  if type(replaced.children) == "table" then
    for index, child in ipairs(replaced.children) do
      replaced.children[index] = tree.map(child, fn)
    end
  end
  return replaced
end

function tree.restyle(node, predicate, style)
  return tree.map(node, function(candidate)
    if not predicate(candidate) then
      return candidate
    end
    if type(candidate.text) == "table" then
      for _, line in ipairs(candidate.text) do
        if type(line) == "table" then
          for _, span in ipairs(line) do
            if type(span) == "table" then
              span.style = style
            end
          end
        end
      end
    end
    return candidate
  end)
end

return tree
