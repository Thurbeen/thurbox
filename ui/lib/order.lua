local order = {}

local function block_end(items, at)
  local last = #items
  local finish = at + 1
  while finish <= last and items[finish].depth > items[at].depth do
    finish = finish + 1
  end
  return finish
end

local function group_start(items, at)
  for index = at, 1, -1 do
    if items[index].header then
      return index
    end
  end
  return #items > 0 and 1 or nil
end

local function group_end(items, at)
  for index = at + 1, #items do
    if items[index].header then
      return index
    end
  end
  return #items + 1
end

local function same_host(items, a, b)
  return items[a].host == items[b].host
end

local function root_ranges(items, at, down)
  local last = #items
  local finish = block_end(items, at)
  local gs = group_start(items, at)
  if not gs then
    return nil
  end
  local ge = group_end(items, gs)

  if down then
    if finish < ge then
      return at, finish, finish, block_end(items, finish)
    elseif ge <= last and same_host(items, gs, ge) then
      return gs, ge, ge, group_end(items, ge)
    end
    return nil
  end
  if at > gs then
    for index = at - 1, gs, -1 do
      if items[index].depth == 0 then
        return index, at, at, finish
      end
    end
    return nil
  end
  if gs > 1 then
    local previous = group_start(items, gs - 1)
    if previous and same_host(items, previous, gs) then
      return previous, gs, gs, ge
    end
  end
  return nil
end

local function child_ranges(items, at, down)
  local last = #items
  local depth = items[at].depth
  local finish = block_end(items, at)

  if down then
    if finish <= last and items[finish].depth == depth then
      return at, finish, finish, block_end(items, finish)
    end
    return nil
  end
  local index = at - 1
  while index >= 1 and items[index].depth > depth do
    index = index - 1
  end
  if index >= 1 and items[index].depth == depth then
    return index, at, at, finish
  end
  return nil
end

function order.move_block(items, at, down)
  local a_start, a_end, b_start, b_end
  if items[at].depth == 0 then
    a_start, a_end, b_start, b_end = root_ranges(items, at, down)
  else
    a_start, a_end, b_start, b_end = child_ranges(items, at, down)
  end
  if not a_start then
    return nil
  end
  local moved = {}
  for index = 1, a_start - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = b_start, b_end - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = a_start, a_end - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = b_end, #items do
    moved[#moved + 1] = items[index]
  end
  return moved
end

local function block_name(block)
  local session = block[1].session
  return session and (session.name or ""):lower() or nil
end

local function emit_blocks(out, blocks)
  for _, block in ipairs(blocks) do
    for _, item in ipairs(block) do
      out[#out + 1] = item
    end
  end
end

function order.sorted_within_groups(items)
  local out = {}
  local at = 1
  while at <= #items do
    local group_last = group_end(items, at) - 1
    local blocks = {}
    local index = at
    while index <= group_last do
      local finish = block_end(items, index)
      local block = {}
      for inner = index, finish - 1 do
        block[#block + 1] = items[inner]
      end
      blocks[#blocks + 1] = block
      index = finish
    end
    local named, nameless = {}, {}
    for position, block in ipairs(blocks) do
      block.position = position
      block.name = block_name(block)
      if block.name then
        named[#named + 1] = block
      else
        nameless[#nameless + 1] = block
      end
    end
    table.sort(named, function(a, b)
      if a.name == b.name then
        return a.position < b.position
      end
      return a.name < b.name
    end)
    emit_blocks(out, named)
    emit_blocks(out, nameless)
    at = group_last + 1
  end
  return out
end

return order
