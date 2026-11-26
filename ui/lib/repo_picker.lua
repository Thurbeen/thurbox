local fuzzy = require("lib.fuzzy")
local widgets = require("lib.widgets")

local repo_picker = {}

local rows_cache = {}

local function collapsed_digest(collapsed)
  local parts = {}
  for path in pairs(collapsed or {}) do
    parts[#parts + 1] = path
  end
  table.sort(parts)
  return table.concat(parts, "\1")
end

local function rank(needle, lowered, row)
  local path = row.path
  local leaf = path:gsub("/+$", ""):match("[^/]+$") or path
  local offset = widgets.chars(path:gsub("/+$", "")) - widgets.chars(leaf)
  local function shifted(positions)
    local out = {}
    for index, position in ipairs(positions) do
      out[index] = position + offset
    end
    return out
  end

  local name = leaf:lower()
  local at = name:find(lowered, 1, true)
  if at then
    local first = widgets.chars(name:sub(1, at - 1)) + 1
    local positions = {}
    for index = 0, widgets.chars(lowered) - 1 do
      positions[#positions + 1] = first + index
    end
    local score = (name == lowered and 400) or (at == 1 and 350) or 300
    return score, shifted(positions)
  end
  local in_leaf = fuzzy.match(needle, leaf)
  if in_leaf then
    return 200, shifted(in_leaf)
  end
  if row.label and fuzzy.match(needle, row.label) then
    return 150, {}
  end
  local in_path = fuzzy.match(needle, path)
  if in_path then
    return 100, in_path
  end
  return nil
end

function repo_picker.rows(published, query, collapsed)
  published = published or {}
  query = query or ""
  local folded = collapsed_digest(collapsed)
  if
    rows_cache.entries
    and rawequal(published, rows_cache.published)
    and query == rows_cache.query
    and folded == rows_cache.folded
  then
    return rows_cache.entries
  end

  local out = {}
  if query ~= "" then
    local needle = fuzzy.compile(query)
    local lowered = query:lower()
    for index, row in ipairs(published) do
      if not row.is_parent then
        local score, matched = rank(needle, lowered, row)
        if score then
          out[#out + 1] = { row = row, matched = matched, score = score, index = index }
        end
      end
    end
    table.sort(out, function(a, b)
      if a.score ~= b.score then
        return a.score > b.score
      end
      return a.index < b.index
    end)
  else
    for _, row in ipairs(published) do
      local folded = row.parent ~= nil and collapsed[row.parent] == true
      if row.is_parent or not folded then
        out[#out + 1] = { row = row, matched = {} }
      end
    end
  end
  rows_cache = { published = published, query = query, folded = folded, entries = out }
  return out
end

function repo_picker.newest(published)
  for _, row in ipairs(published or {}) do
    if not row.offered then
      if row.is_parent then
        return nil
      end
      return row
    end
  end
  return nil
end

function repo_picker.index_of(entries, path)
  for index, entry in ipairs(entries) do
    if entry.row.path == path then
      return index
    end
  end
  return nil
end

function repo_picker.current(entries, cursor)
  if #entries == 0 then
    return nil
  end
  return entries[math.max(1, math.min(cursor or 1, #entries))]
end

function repo_picker.with_worktrees(entries, expanded, published)
  published = published or {}
  if not expanded or published.repo ~= expanded or #(published.list or {}) == 0 then
    return entries
  end
  local out = {}
  for _, entry in ipairs(entries) do
    out[#out + 1] = entry
    if entry.row.path == expanded and not entry.row.is_parent then
      for _, wt in ipairs(published.list) do
        out[#out + 1] = {
          matched = {},
          row = {
            path = wt.path,
            name = (wt.path:gsub("/+$", ""):match("[^/]+$")) or wt.path,
            branch = wt.branch,
            parent = expanded,
            is_parent = false,
            is_worktree = true,
            offered = false,
          },
        }
      end
    end
  end
  return out
end

function repo_picker.chosen(published, selected, worktree)
  selected, worktree = selected or {}, worktree or {}
  local worktrees, plain = {}, {}
  for _, row in ipairs(published or {}) do
    if not row.is_parent and selected[row.path] then
      if worktree[row.path] then
        worktrees[#worktrees + 1] = row.path
      else
        plain[#plain + 1] = row.path
      end
    end
  end
  return worktrees, plain
end

return repo_picker
