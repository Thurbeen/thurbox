-- The repo picker's row model: which remembered repositories to draw, and
-- which were chosen.
--
-- Pure functions over the published bookmark rows and the flow's own choices —
-- the creation flow passes both in, so nothing here reads the snapshot or the
-- flow's other state. Matching is `lib.fuzzy`, the same subsequence match the
-- search strip and the session list use, so the three cannot disagree about
-- what a query hits.

local fuzzy = require("lib.fuzzy")
local widgets = require("lib.widgets")

local repo_picker = {}

--- The rows to draw, in order: every published row, minus the children of a
--- collapsed parent — or, while searching, the matching repositories ranked
--- best first (see `rank`).
---
--- A search expands every group, as v1's does — a match hidden inside a
--- collapsed folder would be unfindable — and leaves the headers out: the
--- ranked list is flat, and a header is a folder rather than a pick.
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

--- How well `row` answers the search, and the path characters to accent; nil
--- when it does not match at all.
---
--- The repository's NAME (its last component) is what a query is aimed at.
--- Every remembered path shares its leading directories, so matching the whole
--- path alone made a query spelled across them match every row, in list order —
--- with the row actually called that somewhere down the list, while the cursor
--- (which `enter` takes) sat on the first. So: the name exactly, then the name
--- starting with the query, then containing it, then as a subsequence; then
--- the label; then the path anywhere, still shown but last.
local function rank(needle, lowered, row)
  local path = row.path
  local leaf = path:gsub("/+$", ""):match("[^/]+$") or path
  -- Where the leaf starts in the path, in characters, so its hits can be
  -- accented where they are drawn.
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
  -- A labelled row is findable by its label too, and the label is what the
  -- reader sees: typing `interface` must reach the interface directory even
  -- though that word appears nowhere in its path. Highlighting stays over the
  -- path, so a label-only hit shows as an unhighlighted match rather than
  -- accenting characters at positions that mean nothing there.
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
    -- Ties keep the published order, which is most recent first.
    table.sort(out, function(a, b)
      if a.score ~= b.score then
        return a.score > b.score
      end
      return a.index < b.index
    end)
  else
    for _, row in ipairs(published) do
      -- A header is always shown: it is the handle its children are folded under.
      local folded = row.parent ~= nil and collapsed[row.parent] == true
      if row.is_parent or not folded then
        out[#out + 1] = { row = row, matched = {} }
      end
    end
  end
  rows_cache = { published = published, query = query, folded = folded, entries = out }
  return out
end

--- Insert a repo's existing worktrees as child rows directly under it.
---
--- `expanded` is the repo path whose worktrees are showing (the one the cursor
--- last rested on), and `published` is `thurbox.worktrees` — used only when it
--- answers for that same repo, so a stale answer for the previous one never
--- draws under the current.
---
--- The children are synthetic rows rather than bookmarks: they carry `branch`
--- and a `worktree` marker, and `path` is git's own path for the checkout,
--- because opening one needs exactly that path.
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

--- The most recently used remembered repository — where a path just added lands.
---
--- Memory is published most-recent-first, so this is "the first row" — but only
--- among the rows that ARE memory. A row the kernel **offers** on its own
--- account (the interface directory) leads the list by construction rather than
--- by recency, so it is stepped over: selecting it is what select-or-add did
--- before, which checked the interface directory for every repository added
--- instead of the one just typed.
function repo_picker.newest(published)
  for _, row in ipairs(published or {}) do
    if not row.offered then
      -- The first row of memory settles it either way. A folder header there
      -- means the newest memory is a folder rather than a repository, and
      -- reaching further down the list would claim one nobody asked for.
      if row.is_parent then
        return nil
      end
      return row
    end
  end
  return nil
end

--- Where a path sits among the rendered rows, or nil when it is not drawn — a
--- search excludes it, or it is folded into a collapsed folder.
function repo_picker.index_of(entries, path)
  for index, entry in ipairs(entries) do
    if entry.row.path == path then
      return index
    end
  end
  return nil
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
