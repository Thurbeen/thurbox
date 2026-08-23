-- Manual-order algebra over the session list's rendered items.
--
-- A move is computed over the RENDERED items and sent as an explicit order.
-- The kernel cannot compute it: only the pane knows the repo grouping and the
-- parent/child nesting, and therefore what a move actually swaps — a root row
-- drags its whole subtree, a root row at its group's edge moves the WHOLE
-- GROUP past the neighbouring one, and a nested child moves among its
-- siblings only. Ported from v1's `move_in_order` and
-- `sort_alphabetically_within_groups`.
--
-- Pure functions over the item list `session_model.build` returns: an item
-- carries `depth` (nesting), `header` (only on a group's first row) and, for a
-- session, `session.name`. Nothing here reads the snapshot or the theme.

local order = {}

--- End of the block rooted at `at`: the first later row at a depth `<=` its own,
--- i.e. one past its whole rendered subtree.
local function block_end(items, at)
  local last = #items
  local finish = at + 1
  while finish <= last and items[finish].depth > items[at].depth do
    finish = finish + 1
  end
  return finish
end

--- Start of the group containing `at`: the nearest row at or above it that
--- carries a header.
---
--- Only the first row of a group carries one, and with `group_by_repo` off
--- nothing does -- so the answer there is row 1: the whole list is one group. It
--- used to be nil, which made `root_ranges` give up and every root move a silent
--- no-op for anyone who had turned grouping off.
local function group_start(items, at)
  for index = at, 1, -1 do
    if items[index].header then
      return index
    end
  end
  return #items > 0 and 1 or nil
end

--- Start of the group after the one starting at `at`, or one past the end.
local function group_end(items, at)
  for index = at + 1, #items do
    if items[index].header then
      return index
    end
  end
  return #items + 1
end

--- The two adjacent ranges a root move swaps: the neighbouring root block in
--- the same group, or — at a group edge — this whole group with its neighbour.
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
    elseif ge <= last then
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
    if previous then
      return previous, gs, gs, ge
    end
  end
  return nil
end

--- The two adjacent ranges a nested move swaps: the adjacent same-depth sibling
--- only, so a child never leaves its parent.
local function child_ranges(items, at, down)
  local last = #items
  local depth = items[at].depth
  local finish = block_end(items, at)

  if down then
    -- A shallower row where the next sibling would start means the parent's
    -- subtree ended.
    if finish <= last and items[finish].depth == depth then
      return at, finish, finish, block_end(items, finish)
    end
    return nil
  end
  -- Scan back over the previous sibling's subtree; a same-depth row is that
  -- sibling, a shallower one is our parent.
  local index = at - 1
  while index >= 1 and items[index].depth > depth do
    index = index - 1
  end
  if index >= 1 and items[index].depth == depth then
    return index, at, at, finish
  end
  return nil
end

--- The rendered item list with the block at `at` moved one place, or nil when
--- it is already at the edge that move would take it past.
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

--- Sort by name **within each repo group**, preserving group order and the
--- parent/child nesting: roots sort among themselves, each parent's children
--- among theirs. v1's `sort_alphabetically_within_groups`.
function order.sorted_within_groups(items)
  local out = {}
  local at = 1
  while at <= #items do
    local group_last = group_end(items, at) - 1
    -- Collect this group's root blocks, each as its own subtree.
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
    -- Case-insensitive, like v1, and stable on a tie so equal names keep their
    -- existing relative order.
    for position, block in ipairs(blocks) do
      block.position = position
    end
    table.sort(blocks, function(a, b)
      local left = (a[1].session.name or ""):lower()
      local right = (b[1].session.name or ""):lower()
      if left == right then
        return a.position < b.position
      end
      return left < right
    end)
    for _, block in ipairs(blocks) do
      for _, item in ipairs(block) do
        out[#out + 1] = item
      end
    end
    at = group_last + 1
  end
  return out
end

return order
