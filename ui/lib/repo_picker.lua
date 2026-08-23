-- The repo picker's row model: which remembered repositories to draw, and
-- which were chosen.
--
-- Pure functions over the published bookmark rows and the flow's own choices —
-- the creation flow passes both in, so nothing here reads the snapshot or the
-- flow's other state. Matching is `lib.fuzzy`, the same subsequence match the
-- search strip and the session list use, so the three cannot disagree about
-- what a query hits.

local fuzzy = require("lib.fuzzy")

local repo_picker = {}

--- The rows to draw, in order: every published row, minus the children of a
--- collapsed parent, minus anything the search excludes.
---
--- A search expands every group, as v1's does — a match hidden inside a
--- collapsed folder would be unfindable.
---
--- Memoized: a single keystroke can ask for the rows several times (the
--- renderer, the cursor lookup in three action branches, the click resolver),
--- and each walk fuzzy-matches every bookmark. The published rows are a gated
--- group, so their table identity keys the memo; the collapsed set is tiny and
--- digested by value because `state` hands back a fresh table on every read.
local rows_cache = {}

local function collapsed_digest(collapsed)
  local parts = {}
  for path in pairs(collapsed or {}) do
    parts[#parts + 1] = path
  end
  table.sort(parts)
  return table.concat(parts, "\1")
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

  local searching = query ~= ""
  local needle = fuzzy.compile(query)
  local out = {}
  for _, row in ipairs(published) do
    local hidden = row.parent ~= nil and not searching and collapsed[row.parent] == true
    -- Spelled as a branch rather than `searching and match(...) or {}`: a MISS is
    -- nil, and `nil or {}` is an empty table — which reads as "matched nothing"
    -- and would include every row the search excludes.
    local matched
    if searching then
      matched = fuzzy.match(needle, row.path)
      -- A labelled row is findable by its label too, and the label is what the
      -- reader sees: typing `interface` must reach the interface directory even
      -- though that word appears nowhere in its path. Highlighting stays over the
      -- path, so a label-only hit shows as an unhighlighted match rather than
      -- accenting characters at positions that mean nothing there.
      if not matched and row.label and fuzzy.match(needle, row.label) then
        matched = {}
      end
    else
      matched = {}
    end
    -- A header is always shown: it is the handle its children are folded under.
    if row.is_parent then
      out[#out + 1] = { row = row, matched = {} }
    elseif not hidden and matched then
      out[#out + 1] = { row = row, matched = matched }
    end
  end
  rows_cache = { published = published, query = query, folded = folded, entries = out }
  return out
end

--- The entry the cursor is on, or nil when the list is empty.
function repo_picker.current(entries, cursor)
  if #entries == 0 then
    return nil
  end
  return entries[math.max(1, math.min(cursor or 1, #entries))]
end

--- Split the chosen repositories the way v1's `partition_selected_repos` does:
--- the ones taking a worktree, then the ones attached as they are.
function repo_picker.chosen(published, selected, worktree)
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
