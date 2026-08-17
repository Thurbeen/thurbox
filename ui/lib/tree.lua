-- Walking a view tree.
--
-- The userland half of decoration: a decorator receives another plugin's tree
-- and returns a modified one, so it needs to find the nodes it cares about. It
-- does that by the identity nodes already carry — no selector engine, because
-- the consumer wants "rows whose text contains the query" and a matching
-- language would be a great deal of kernel for that.
--
-- If this ever needs to be fast, this file is where a shared implementation
-- lives, and only then is it a candidate for moving into the kernel.

local tree = {}

--- Does a node carry `class`?
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

--- All the text in a node, flattened — what a search matches against.
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

--- Apply `fn` to every node in the tree, depth first, in place.
---
--- `fn` returns a node (usually the one it was given, modified). Returning nil
--- leaves the node untouched, so a decorator that only cares about rows can
--- ignore everything else.
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

--- Restyle every node satisfying `predicate`.
function tree.restyle(node, predicate, style)
  return tree.map(node, function(candidate)
    if not predicate(candidate) then
      return candidate
    end
    -- Text is a list of lines, each a list of spans; style every span so the
    -- whole row reads as matched.
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
